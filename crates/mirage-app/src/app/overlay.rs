//! User overlay (iteration 2 §1): settings layered over the policy output,
//! hot-applied to the running core when they change.
use super::runtime::{self, OverlayOutcome};
use super::state::{Notice, RuntimeStatus};
use super::{AppCore, Internal, info_notice};
use crate::cores::mihomo::compose::UserOverlay;

impl AppCore {
    pub(super) fn user_overlay(&self) -> UserOverlay {
        UserOverlay {
            ipv6: self.settings.ipv6,
            unified_delay: self.settings.unified_delay,
        }
    }

    /// Re-composes the running revision when the saved overlay differs from
    /// the one the core runs. Waits while a start or reload is in flight;
    /// those paths call back here when they finish.
    pub(super) fn sync_user_overlay(&mut self) {
        if self.view.runtime != RuntimeStatus::Running {
            return;
        }
        let wanted = self.user_overlay();
        let Some(running) = &self.running else { return };
        if running.user == wanted {
            return;
        }
        let profile_id = running.profile_id.clone();
        let (Some(binary), Some(rev)) = (
            self.binary.clone(),
            self.profiles
                .get(&profile_id)
                .and_then(|p| p.active_revision.clone()),
        ) else {
            return;
        };
        let Some(req) = self.reload_request(&profile_id, rev) else {
            return;
        };
        let Some(running) = self.running.as_mut() else {
            return;
        };
        let previous = std::mem::replace(&mut running.user, req.user.clone());
        let attempted = req.user.clone();
        let generation = self.generation;
        self.reset_delay_tests();
        self.suspend_connection_stream();
        self.view.runtime = RuntimeStatus::Reloading;
        tracing::info!(profile = %profile_id, "overlay reload started");
        self.spawn(async move {
            Internal::OverlayApplied {
                generation,
                attempted,
                previous: previous.clone(),
                outcome: runtime::reapply(req, binary, previous).await,
            }
        });
    }

    pub(super) fn on_overlay_applied(
        &mut self,
        generation: u64,
        attempted: UserOverlay,
        previous: UserOverlay,
        outcome: OverlayOutcome,
    ) {
        if generation != self.generation {
            // Stopped or restarted meanwhile; the next start uses the saved settings.
            return;
        }
        if self.view.runtime == RuntimeStatus::Reloading {
            self.view.runtime = RuntimeStatus::Running;
        }
        let name = self
            .running
            .as_ref()
            .map(|r| self.profile_log_name(&r.profile_id))
            .unwrap_or_default();
        let (reason, restart) = match outcome {
            OverlayOutcome::Applied => {
                tracing::info!("overlay reload applied");
                self.record_operation(
                    "应用设置",
                    &name,
                    "手动",
                    "成功",
                    &format!("已热重载 · {}", attempted.describe()),
                );
                self.view.notice = Some(info_notice("设置已保存并热重载"));
                self.finish_overlay_reload();
                return;
            }
            OverlayOutcome::Rejected(reason) => (format!("内核校验未通过：{reason}"), false),
            OverlayOutcome::RolledBack(reason) => (format!("热重载失败：{reason}"), false),
            OverlayOutcome::NeedsRestart(reason) => (format!("热重载失败：{reason}"), true),
        };
        if let Some(running) = self.running.as_mut() {
            running.user = previous.clone();
        }
        // Restore the settings unless the user already changed them again.
        if self.user_overlay() == attempted {
            self.settings.ipv6 = previous.ipv6;
            self.settings.unified_delay = previous.unified_delay;
            if let Err(e) = self.save_settings_file() {
                tracing::warn!(error = %e, "overlay settings not restored");
            }
            self.refresh_settings_view();
        }
        let tail = if restart {
            "已恢复原设置，正在重启内核"
        } else {
            "已恢复原设置"
        };
        self.record_operation(
            "应用设置",
            &name,
            "手动",
            "失败",
            &format!("{reason}；{tail}"),
        );
        self.view.notice = Some(Notice::error(format!("{reason}；{tail}")));
        if restart && let Some(running) = self.running.take() {
            self.view.groups.clear();
            self.start_after(Some(running));
            return;
        }
        self.finish_overlay_reload();
    }

    fn finish_overlay_reload(&mut self) {
        if self.running.is_some() {
            self.restart_live_streams();
            self.refresh_groups();
        }
        self.commit_queued_revision();
    }
}
