//! Revision commits, serialized hot reload and last-good rollback.
use super::runtime::ReloadOutcome;
use super::state::{Notice, RuntimeStatus};
use super::{AppCore, Internal, info_notice, runtime, updates::profile_time};
use crate::domain::model::{FetchRecord, ProfileId, RevisionId, Validity, now};
use crate::subscriptions::revisions;

impl AppCore {
    /// §8 step 7: a profile that is not running just switches its active
    /// revision; the running one goes through a hot reload first.
    pub(super) fn commit_candidate(
        &mut self,
        profile_id: &ProfileId,
        rev: RevisionId,
        manual: bool,
    ) {
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
        let saved = self.mark_good(profile_id, &rev);
        self.complete_revision_log(profile_id, &rev, saved, "新版本已保存，下次连接使用");
        if manual && let Some(p) = self.profiles.get(profile_id) {
            self.view.notice = Some(info_notice(format!("\"{}\" 已更新", p.name)));
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
            allow_lan: running.allow_lan,
            mode: self.settings.routing_mode,
            fallback: profile.last_good_revision.clone(),
            candidate: candidate.clone(),
            tun: running.process.service().is_some(),
            service: running.process.service(),
        };
        let generation = self.generation;
        let profile_id = profile_id.clone();
        self.reset_delay_tests();
        self.suspend_connection_stream();
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
        if self.profiles.get(&profile_id).is_none() {
            return;
        }
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
                let saved = self.mark_good(&profile_id, &revision);
                self.complete_revision_log(&profile_id, &revision, saved, "新配置已应用");
                if current {
                    self.view.notice = Some(info_notice(format!(
                        "\"{name}\" 已更新并热重载，节点选择已恢复"
                    )));
                }
            }
            ReloadOutcome::RolledBack(reason) => {
                if generation != self.generation {
                    // Disconnected/stopped; the reload task is stale, no notice.
                    return;
                }
                self.finish_update_log(
                    &profile_id,
                    "失败",
                    &format!("新配置应用失败: {reason}; 已回退到原版本"),
                );
                self.mark_invalid(&profile_id, &revision, &reason);
                self.view.notice = Some(Notice::error(format!(
                    "\"{name}\" 新版本应用失败, 已回退到上一个可用版本: {reason}"
                )));
            }
            ReloadOutcome::NeedsRestart(reason) => {
                if generation != self.generation {
                    // Disconnected/stopped; the reload task is stale, no notice.
                    return;
                }
                self.finish_update_log(
                    &profile_id,
                    "失败",
                    &format!("新配置应用失败: {reason}; 正在使用原版本重启"),
                );
                self.mark_invalid(&profile_id, &revision, &reason);
                self.view.notice = Some(Notice::error(format!(
                    "\"{name}\" 新版本应用失败, 正在用上一个可用版本重启内核: {reason}"
                )));
                if current && let Some(previous) = self.running.take() {
                    self.view.groups.clear();
                    self.start_after(Some(previous));
                }
            }
        }
        if current && self.running.is_some() {
            self.restart_live_streams();
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
    fn mark_good(&mut self, profile_id: &ProfileId, rev: &RevisionId) -> bool {
        let Some(profile) = self.profiles.get_mut(profile_id) else {
            return false;
        };
        profile.active_revision = Some(rev.clone());
        profile.last_good_revision = Some(rev.clone());
        profile.updated_at = now();
        let profile = profile.clone();
        let saved = self.save_profiles();
        // Re-prune now that the pinned revisions changed.
        if let Ok(Some(r)) = revisions::find(&self.layout, &profile, rev)
            && let Err(e) = revisions::upsert(&self.layout, &profile, r)
        {
            tracing::warn!(error = %e, "revision prune failed");
        }
        saved
    }

    fn complete_revision_log(&mut self, id: &str, rev: &str, saved: bool, text: &str) {
        let summary = self
            .profiles
            .get(id)
            .and_then(|p| {
                revisions::find(&self.layout, p, &rev.to_owned())
                    .ok()
                    .flatten()
            })
            .map(|r| {
                format!(
                    "{} 个节点 · {} 个组 · {} 条规则 · {text}{}",
                    r.summary.proxies,
                    r.summary.groups,
                    r.summary.rules,
                    if r.policy_notes.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", r.policy_notes.join("；"))
                    }
                )
            })
            .unwrap_or_else(|| text.into());
        self.finish_update_log(
            id,
            if saved { "成功" } else { "失败" },
            if saved {
                &summary
            } else {
                "配置保存失败"
            },
        );
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
}
