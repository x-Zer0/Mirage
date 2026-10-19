//! URL subscriptions in the actor (DESIGN.md §8, §15, §16.4): adding,
//! manual/scheduled downloads and completion handling.
//!
//! Downloads run concurrently (at most 3); reload.rs handles serialized
//! revision commits, hot reload and rollback.

use super::accounts;
use super::pipeline::{self, RemoteAdded, RemoteFetch, UpdateOutcome, UpdateResult};
use super::state::{Notice, Retry, stage_label};
use super::{AppCore, Internal, error_notice, info_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{
    FetchRecord, Profile, ProfileId, SubscriptionSource, UpdatePolicy, now,
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
                self.record_operation(
                    "新增订阅",
                    &name,
                    "手动 · 订阅链接",
                    "失败",
                    &e.user_message(),
                );
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
        self.view.busy_tasks.insert("正在下载并校验订阅…".into());
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
        self.view.busy_tasks.clear();
        let display_name = if added.name.is_empty() {
            net::parse_url(added.url.expose())
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "订阅链接".into())
        } else {
            added.name.clone()
        };
        let outcome = match added.result {
            Ok(o) => o,
            Err(e) => {
                self.record_operation(
                    "新增订阅",
                    &display_name,
                    "手动 · 订阅链接",
                    "失败",
                    &e.user_message(),
                );
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
            // Clean up revision and directory since we're aborting
            let raw_path = self
                .layout
                .raw_revision(&outcome.profile_id, &outcome.revision.id);
            let _ = std::fs::remove_file(&raw_path);
            let profile_dir = self.layout.profile_dir(&outcome.profile_id);
            let _ = std::fs::remove_dir_all(&profile_dir);
            self.record_operation(
                "新增订阅",
                &name,
                "手动 · 订阅链接",
                "失败",
                &e.user_message(),
            );
            self.view.notice = Some(error_notice(&e));
            return;
        }
        let source = SubscriptionSource::RemoteUrl {
            url_secret: url_secret.clone(),
            host,
            user_agent: None,
        };
        if !self.store_new_profile(outcome, name, source) {
            // Rollback: remove the URL secret we just stored
            let _ = self.secrets.delete(&url_secret);
        }
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
            self.record_operation("新增订阅", &name, "手动", "失败", &e.user_message());
            self.view.notice = Some(error_notice(&e));
            return false;
        }
        let origin = match &profile.source {
            SubscriptionSource::RemoteUrl { host, .. } => format!("订阅链接 · {host}"),
            SubscriptionSource::ProviderAccount { provider_id, .. } => {
                format!("账号订阅 · {}", crate::providers::account_name(provider_id))
            }
            other => other.label().into(),
        };
        let description = format!(
            "{} · {} 个节点 · {} 个组 · {} 条规则{}",
            origin,
            outcome.revision.summary.proxies,
            outcome.revision.summary.groups,
            outcome.revision.summary.rules,
            if outcome.revision.policy_notes.is_empty() {
                String::new()
            } else {
                format!(" · {}", outcome.revision.policy_notes.join("；"))
            }
        );
        self.profiles.add(profile.clone());
        if !self.save_profiles() {
            // Rollback: remove from profiles list
            self.profiles.remove(&profile.id);
            // Clean up stored revision and directory
            let raw_path = self.layout.raw_revision(&profile.id, &outcome.revision.id);
            let _ = std::fs::remove_file(&raw_path);
            let profile_dir = self.layout.profile_dir(&profile.id);
            let _ = std::fs::remove_dir_all(&profile_dir);
            self.record_operation("新增订阅", &name, "手动", "失败", "配置列表保存失败");
            return false;
        }
        self.record_operation("新增订阅", &name, "手动", "成功", &description);
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
        if self.updating.contains(id) || self.update_operations.contains_key(id) {
            return;
        }
        let Some(profile) = self.profiles.get(id).cloned() else {
            return;
        };
        if !profile.is_remote() {
            return;
        }
        if self.view.login_busy && accounts::account_of(&profile).is_some() {
            return;
        }
        let Some(binary) = self.require_binary() else {
            return;
        };
        self.begin_update_log(id, manual);
        let source = match self.update_source(&profile) {
            Ok(Some(source)) => source,
            Ok(None) => {
                self.finish_update_log(&profile.id, "成功", "本地文件配置无需更新");
                if manual {
                    self.view.notice = Some(info_notice("本地文件配置无需更新"));
                }
                return;
            }
            Err(e) => {
                self.finish_update_log(&profile.id, "失败", &e.user_message());
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
            self.finish_update_log(&outcome.profile_id, "失败", "配置已删除，更新已取消");
            // Removed while downloading: drop what the task may have written.
            let _ = std::fs::remove_dir_all(self.layout.profile_dir(&outcome.profile_id));
            self.refresh_profiles_view();
            return;
        };
        if let Some(meta) = &outcome.meta {
            apply_meta(profile, meta);
        }
        let name = profile.name.clone();
        let mut completed = None;
        match outcome.result {
            Err(e) => {
                completed = Some(("失败", format!("{}；继续使用原版本", e.user_message())));
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
                completed = Some(("成功", "已是最新，内容未变化".to_owned()));
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
        let saved = self.save_profiles();
        if let Some((status, summary)) = completed {
            self.finish_update_log(
                &outcome.profile_id,
                if saved { status } else { "失败" },
                if saved {
                    &summary
                } else {
                    "更新结果保存失败"
                },
            );
        }
        self.refresh_profiles_view();
    }

    fn load_url(&self, key: &str) -> AppResult<reqwest::Url> {
        let bytes = self.secrets.get(key)?.ok_or_else(|| {
            AppError::new(
                AppErrorKind::Storage,
                STAGE,
                "找不到已保存的订阅地址，请删除后重新添加",
            )
        })?;
        let text = String::from_utf8(bytes.to_vec())
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

    pub(super) fn save_profiles(&mut self) -> bool {
        if let Err(e) = self.profiles.save(&self.layout) {
            self.view.notice = Some(error_notice(&e));
            return false;
        }
        true
    }

    /// Profile removal clears its URL or the associated provider account.
    pub(super) fn forget_secrets(&mut self, profile: &Profile) -> AppResult<()> {
        match &profile.source {
            SubscriptionSource::RemoteUrl { url_secret, .. } => self.secrets.delete(url_secret),
            SubscriptionSource::ProviderAccount { account_id, .. } => {
                self.remove_account(account_id)
            }
            SubscriptionSource::LocalFile { .. } => Ok(()),
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
