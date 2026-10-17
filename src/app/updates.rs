//! URL subscriptions in the actor (DESIGN.md §8, §15, §16.4): adding,
//! manual/scheduled updates and committing a new revision to the running core.
//!
//! Downloads run concurrently (at most 3); committing is a transaction: only
//! one hot reload at a time, a newer candidate for the running profile waits.

use super::accounts;
use super::pipeline::{self, RemoteAdded, RemoteFetch, UpdateOutcome, UpdateResult};
use super::runtime::{self, ReloadOutcome};
use super::state::{Notice, Retry, RuntimeStatus, stage_label};
use super::{AppCore, Internal, error_notice, info_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{
    FetchRecord, Profile, ProfileId, RevisionId, SubscriptionSource, UpdatePolicy, Validity, now,
};
use crate::domain::redact::SecretString;
use crate::net;
use crate::subscriptions::fetch::Descriptor;
use crate::subscriptions::revisions;
use crate::subscriptions::userinfo::SubscriptionMeta;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const STAGE: &str = "app.updates";
/// §6.5: subscription downloads may run concurrently, at most three.
pub const MAX_PARALLEL_FETCH: usize = 3;
/// How often due profiles are looked for.
pub const SCHEDULE_TICK: Duration = Duration::from_secs(60);
/// A failed scheduled update is retried after at most this long.
const RETRY_AFTER_FAILURE: i64 = 3600;

pub struct AddedRemote {
    pub name: String,
    pub url: SecretString,
    pub result: AppResult<RemoteAdded>,
}

impl AppCore {
    pub(super) fn add_remote(&mut self, name: String, url: SecretString) {
        let Some(binary) = self.require_binary() else {
            return;
        };
        let parsed = match net::parse_url(url.expose()) {
            Ok(u) => u,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        let req = RemoteFetch {
            binary,
            layout: self.layout.clone(),
            desc: Descriptor::plain(parsed, self.settings.default_user_agent.clone()),
            egress: UpdatePolicy::default().egress,
            core_port: self.core_port(),
            mixed_port: self.view.mixed_port,
        };
        self.view.busy = Some("正在下载并校验订阅…".into());
        let permits = self.fetch_permits.clone();
        self.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let result = pipeline::add_remote(req).await;
            Internal::RemoteAdded(Box::new(AddedRemote {
                name: name.trim().to_owned(),
                url,
                result,
            }))
        });
    }

    pub(super) fn on_remote_added(&mut self, added: AddedRemote) {
        self.view.busy = None;
        let outcome = match added.result {
            Ok(o) => o,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        let host = net::parse_url(added.url.expose())
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        let name = Some(added.name)
            .filter(|n| !n.is_empty())
            .or_else(|| outcome.meta.filename.clone())
            .unwrap_or_else(|| host.clone());
        let url_secret = format!("profile/{}/url", outcome.profile_id);
        if let Err(e) = self.secrets.put(&url_secret, added.url.expose().as_bytes()) {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        let source = SubscriptionSource::RemoteUrl {
            url_secret,
            host,
            user_agent: None,
        };
        self.store_new_profile(outcome, name, source);
    }

    /// Stores the first revision of a downloaded profile and adds it.
    /// Returns false (with a notice) when storing failed.
    pub(super) fn store_new_profile(
        &mut self,
        outcome: RemoteAdded,
        name: String,
        source: SubscriptionSource,
    ) -> bool {
        let mut profile = Profile {
            id: outcome.profile_id.clone(),
            name: name.clone(),
            source,
            active_revision: Some(outcome.revision.id.clone()),
            last_good_revision: Some(outcome.revision.id.clone()),
            updated_at: now(),
            update_policy: UpdatePolicy::default(),
            usage: None,
            last_fetch: None,
        };
        apply_meta(&mut profile, &outcome.meta);
        let stored =
            revisions::store_raw(&self.layout, &profile, &outcome.revision.id, &outcome.raw)
                .and_then(|()| revisions::upsert(&self.layout, &profile, outcome.revision.clone()));
        if let Err(e) = stored {
            self.view.notice = Some(error_notice(&e));
            return false;
        }
        self.profiles.add(profile);
        self.save_profiles();
        self.view.policy_notes = outcome.revision.policy_notes.clone();
        self.view.check_lines = outcome.check_lines;
        let s = outcome.revision.summary;
        self.view.notice = Some(info_notice(format!(
            "已添加“{name}”：{} 个节点，{} 个组，{} 条规则",
            s.proxies, s.groups, s.rules
        )));
        self.refresh_profiles_view();
        true
    }

    pub(super) fn update_profile(&mut self, id: &str, manual: bool) {
        if self.updating.contains(id) {
            return;
        }
        let Some(profile) = self.profiles.get(id).cloned() else {
            return;
        };
        let Some(binary) = self.require_binary() else {
            return;
        };
        let source = match self.update_source(&profile) {
            Ok(Some(source)) => source,
            Ok(None) => {
                if manual {
                    self.view.notice = Some(info_notice("本地文件配置无需更新"));
                }
                return;
            }
            Err(e) => {
                self.record_fetch_error(&profile.id, &e, manual);
                return;
            }
        };
        let layout = self.layout.clone();
        let egress = profile.update_policy.egress;
        let core_port = self.core_port();
        let mixed_port = self.view.mixed_port;
        let make_req = move |desc| RemoteFetch {
            binary,
            layout,
            desc,
            egress,
            core_port,
            mixed_port,
        };
        tracing::info!(profile = %profile.id, manual, "subscription update started");
        self.updating.insert(profile.id.clone());
        let permits = self.fetch_permits.clone();
        let tx = self.internal_tx.clone();
        let provider_egress = self.provider_egress();
        let layout = self.layout.clone();
        self.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let desc = match source {
                UpdateSource::Url(desc) => Ok(desc),
                UpdateSource::Provider(plan) => {
                    accounts::resolve(*plan, &layout, provider_egress, &tx).await
                }
            };
            let outcome = match desc {
                Ok(desc) => pipeline::update_remote(make_req(desc), profile).await,
                Err(e) => UpdateOutcome {
                    profile_id: profile.id,
                    meta: None,
                    result: Err(e),
                },
            };
            Internal::Updated(Box::new(outcome), manual)
        });
        self.refresh_profiles_view();
    }

    /// Where an update downloads from; `None` for local files.
    fn update_source(&self, profile: &Profile) -> AppResult<Option<UpdateSource>> {
        match &profile.source {
            SubscriptionSource::LocalFile { .. } => Ok(None),
            SubscriptionSource::RemoteUrl {
                url_secret,
                user_agent,
                ..
            } => {
                let url = self.load_url(url_secret)?;
                let ua = user_agent
                    .clone()
                    .unwrap_or_else(|| self.settings.default_user_agent.clone());
                Ok(Some(UpdateSource::Url(Descriptor::plain(url, ua))))
            }
            SubscriptionSource::ProviderAccount { account_id, .. } => self
                .provider_plan(account_id)
                .map(|plan| Some(UpdateSource::Provider(Box::new(plan)))),
        }
    }

    /// An update that could not even start (e.g. the account needs a login).
    fn record_fetch_error(&mut self, profile_id: &str, e: &AppError, manual: bool) {
        let Some(profile) = self.profiles.get_mut(profile_id) else {
            return;
        };
        let text = format!(
            "更新失败（仍使用 {} 的版本）：{}",
            profile_time(profile.updated_at),
            e.user_message()
        );
        tracing::warn!(error = %e, profile = %profile_id, "subscription update not started");
        profile.last_fetch = Some(FetchRecord {
            at: now(),
            error: Some(text.clone()),
        });
        let name = profile.name.clone();
        if manual {
            self.view.notice = Some(
                Notice::error(format!("“{name}” {text}"))
                    .with_stage(stage_label(e.stage))
                    .with_retry(Retry::UpdateProfile(profile_id.to_owned())),
            );
        }
        self.save_profiles();
        self.refresh_profiles_view();
    }

    pub(super) fn update_all(&mut self) {
        let ids: Vec<ProfileId> = self
            .profiles
            .all()
            .iter()
            .filter(|p| p.is_remote())
            .map(|p| p.id.clone())
            .collect();
        if ids.is_empty() {
            self.view.notice = Some(info_notice("没有可更新的订阅"));
        }
        for id in ids {
            self.update_profile(&id, true);
        }
    }

    /// Starts updates for profiles whose interval elapsed (§2: default 24 h).
    pub(super) fn schedule_due_updates(&mut self) {
        if self.binary.is_none() {
            return;
        }
        let at = now();
        let due: Vec<ProfileId> = self
            .profiles
            .all()
            .iter()
            .filter(|p| p.is_remote() && !self.updating.contains(&p.id) && is_due(p, at))
            .map(|p| p.id.clone())
            .collect();
        for id in due {
            self.update_profile(&id, false);
        }
    }

    pub(super) fn on_updated(&mut self, outcome: UpdateOutcome, manual: bool) {
        self.updating.remove(&outcome.profile_id);
        let Some(profile) = self.profiles.get_mut(&outcome.profile_id) else {
            // Removed while downloading: drop what the task may have written.
            let _ = std::fs::remove_dir_all(self.layout.profile_dir(&outcome.profile_id));
            self.refresh_profiles_view();
            return;
        };
        if let Some(meta) = &outcome.meta {
            apply_meta(profile, meta);
        }
        let name = profile.name.clone();
        match outcome.result {
            Err(e) => {
                let since = profile_time(profile.updated_at);
                let text = format!("更新失败（仍使用 {since} 的版本）：{}", e.user_message());
                tracing::warn!(error = %e, profile = %profile.id, "subscription update failed");
                profile.last_fetch = Some(FetchRecord {
                    at: now(),
                    error: Some(text.clone()),
                });
                if manual {
                    self.view.notice = Some(
                        Notice::error(format!("“{name}” {text}"))
                            .with_stage(stage_label(e.stage))
                            .with_retry(Retry::UpdateProfile(outcome.profile_id.clone())),
                    );
                }
            }
            Ok(UpdateResult::Unchanged) => {
                profile.last_fetch = Some(FetchRecord {
                    at: now(),
                    error: None,
                });
                profile.updated_at = now();
                if manual {
                    self.view.notice = Some(info_notice(format!("“{name}”已是最新")));
                }
            }
            Ok(UpdateResult::Candidate {
                revision,
                check_lines,
            }) => {
                profile.last_fetch = Some(FetchRecord {
                    at: now(),
                    error: None,
                });
                self.view.policy_notes = revision.policy_notes.clone();
                self.view.check_lines = check_lines;
                let id = outcome.profile_id.clone();
                self.commit_candidate(&id, revision.id, manual);
            }
        }
        self.save_profiles();
        self.refresh_profiles_view();
    }

    /// §8 step 7: a profile that is not running just switches its active
    /// revision; the running one goes through a hot reload first.
    fn commit_candidate(&mut self, profile_id: &ProfileId, rev: RevisionId, manual: bool) {
        if matches!(
            self.view.runtime,
            RuntimeStatus::Starting | RuntimeStatus::Restarting
        ) && self.profiles.selected_id() == Some(profile_id)
        {
            self.queued_reload = Some((profile_id.clone(), rev));
            return;
        }
        let running_this = self
            .running
            .as_ref()
            .is_some_and(|r| &r.profile_id == profile_id);
        if running_this && self.view.runtime.is_up() {
            if self.view.runtime == RuntimeStatus::Reloading {
                // One commit at a time; the newest candidate wins.
                self.queued_reload = Some((profile_id.clone(), rev));
                return;
            }
            self.begin_reload(profile_id, rev);
            return;
        }
        self.mark_good(profile_id, &rev);
        if manual && let Some(p) = self.profiles.get(profile_id) {
            self.view.notice = Some(info_notice(format!("“{}”已更新", p.name)));
        }
    }

    fn begin_reload(&mut self, profile_id: &ProfileId, candidate: RevisionId) {
        let (Some(running), Some(profile)) = (&self.running, self.profiles.get(profile_id)) else {
            return;
        };
        let req = runtime::ReloadRequest {
            layout: self.layout.clone(),
            profile: profile.clone(),
            controller: running.controller.clone(),
            controller_addr: running.controller_addr,
            secret: running.secret.clone(),
            mixed_port: running.mixed_port,
            mode: self.settings.routing_mode,
            fallback: profile.last_good_revision.clone(),
            candidate: candidate.clone(),
            tun: running.process.service().is_some(),
            service: running.process.service(),
        };
        let generation = self.generation;
        let profile_id = profile_id.clone();
        self.view.runtime = RuntimeStatus::Reloading;
        tracing::info!(profile = %profile_id, "hot reload started");
        self.spawn(async move {
            let outcome = runtime::reload(req).await;
            Internal::Reloaded {
                generation,
                profile_id,
                revision: candidate,
                outcome,
            }
        });
    }

    pub(super) fn on_reloaded(
        &mut self,
        generation: u64,
        profile_id: ProfileId,
        revision: RevisionId,
        outcome: ReloadOutcome,
    ) {
        let current = generation == self.generation;
        if current && self.view.runtime == RuntimeStatus::Reloading {
            self.view.runtime = RuntimeStatus::Running;
        }
        let name = self
            .profiles
            .get(&profile_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        match outcome {
            ReloadOutcome::Applied => {
                tracing::info!(profile = %profile_id, "hot reload applied");
                self.mark_good(&profile_id, &revision);
                if current {
                    self.view.notice = Some(info_notice(format!(
                        "“{name}”已更新并热重载，节点选择已恢复"
                    )));
                }
            }
            ReloadOutcome::RolledBack(reason) => {
                self.mark_invalid(&profile_id, &revision, &reason);
                self.view.notice = Some(Notice::error(format!(
                    "“{name}”新版本应用失败，已回退到上一个可用版本：{reason}"
                )));
            }
            ReloadOutcome::NeedsRestart(reason) => {
                self.mark_invalid(&profile_id, &revision, &reason);
                self.view.notice = Some(Notice::error(format!(
                    "“{name}”新版本应用失败，正在用上一个可用版本重启内核：{reason}"
                )));
                if current && let Some(previous) = self.running.take() {
                    self.view.groups.clear();
                    self.start_after(Some(previous));
                }
            }
        }
        if current && self.running.is_some() {
            self.refresh_groups();
        }
        self.save_profiles();
        self.refresh_profiles_view();
        self.commit_queued_revision();
    }

    pub(super) fn commit_queued_revision(&mut self) {
        if let Some((id, rev)) = self.queued_reload.take() {
            self.commit_candidate(&id, rev, false);
        }
    }

    /// Applied (or not running): becomes active and last_good (§8 step 8).
    fn mark_good(&mut self, profile_id: &ProfileId, rev: &RevisionId) {
        let Some(profile) = self.profiles.get_mut(profile_id) else {
            return;
        };
        profile.active_revision = Some(rev.clone());
        profile.last_good_revision = Some(rev.clone());
        profile.updated_at = now();
        let profile = profile.clone();
        self.save_profiles();
        // Re-prune now that the pinned revisions changed.
        if let Ok(Some(r)) = revisions::find(&self.layout, &profile, rev)
            && let Err(e) = revisions::upsert(&self.layout, &profile, r)
        {
            tracing::warn!(error = %e, "revision prune failed");
        }
    }

    fn mark_invalid(&mut self, profile_id: &ProfileId, rev: &RevisionId, reason: &str) {
        let Some(profile) = self.profiles.get_mut(profile_id) else {
            return;
        };
        profile.last_fetch = Some(FetchRecord {
            at: now(),
            error: Some(format!(
                "新版本应用失败（仍使用 {} 的版本）：{reason}",
                profile_time(profile.updated_at)
            )),
        });
        let profile = profile.clone();
        match revisions::find(&self.layout, &profile, rev) {
            Ok(Some(mut r)) => {
                r.validity = Validity::Invalid(reason.to_owned());
                if let Err(e) = revisions::upsert(&self.layout, &profile, r) {
                    tracing::warn!(error = %e, "revision not marked invalid");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "revision lookup failed"),
        }
    }

    fn load_url(&self, key: &str) -> AppResult<reqwest::Url> {
        let bytes = self.secrets.get(key)?.ok_or_else(|| {
            AppError::new(
                AppErrorKind::Storage,
                STAGE,
                "找不到已保存的订阅地址，请删除后重新添加",
            )
        })?;
        let text = String::from_utf8(bytes)
            .map_err(|_| AppError::new(AppErrorKind::Storage, STAGE, "订阅地址已损坏"))?;
        net::parse_url(&text)
    }

    /// Mixed port of an up core, for the Auto/ViaLocalCore egress (§12.2).
    pub(super) fn core_port(&self) -> Option<u16> {
        self.running
            .as_ref()
            .filter(|_| self.view.runtime.is_up())
            .map(|r| r.mixed_port)
    }

    pub(super) fn save_profiles(&mut self) {
        if let Err(e) = self.profiles.save(&self.layout) {
            self.view.notice = Some(error_notice(&e));
        }
    }

    /// Removes the stored URL of a deleted profile (§14.1). Account-backed
    /// profiles keep the account; only the link to it goes.
    pub(super) fn forget_secrets(&mut self, profile: &Profile) {
        match &profile.source {
            SubscriptionSource::RemoteUrl { url_secret, .. } => {
                if let Err(e) = self.secrets.delete(url_secret) {
                    tracing::warn!(error = %e, "subscription secret not deleted");
                }
            }
            SubscriptionSource::ProviderAccount { .. } => self.unlink_profile(&profile.id),
            SubscriptionSource::LocalFile { .. } => {}
        }
    }
}

enum UpdateSource {
    Url(Descriptor),
    Provider(Box<accounts::ProviderPlan>),
}

/// Usage and the server's update interval (unless the user chose one).
/// Account-backed profiles take usage from the provider API instead (§15).
fn apply_meta(profile: &mut Profile, meta: &SubscriptionMeta) {
    if meta.usage.is_some() && accounts::account_of(profile).is_none() {
        profile.usage = meta.usage;
    }
    if let Some(hours) = meta.update_interval
        && !profile.update_policy.user_set
    {
        profile.update_policy.interval_hours = hours;
    }
}

fn is_due(p: &Profile, at: i64) -> bool {
    let interval = i64::from(p.update_policy.interval_hours) * 3600;
    if interval == 0 {
        return false;
    }
    match &p.last_fetch {
        Some(FetchRecord {
            at: last,
            error: Some(_),
        }) => at - last >= interval.min(RETRY_AFTER_FAILURE),
        Some(FetchRecord { at: last, .. }) => at - last >= interval,
        None => at - p.updated_at >= interval,
    }
}

pub fn profile_time(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Shared limiter handed to every download task.
pub fn fetch_permits() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(MAX_PARALLEL_FETCH))
}
