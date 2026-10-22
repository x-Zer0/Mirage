//! User overlay (iteration 2 §1): settings layered over the policy output,
//! hot-applied to the running core when they change.
use super::runtime::{self, OverlayOutcome};
use super::state::{Notice, RuntimeStatus};
use super::{AppCore, Internal, info_notice};
use crate::cores::mihomo::compose::UserOverlay;

impl AppCore {
    pub(super) fn user_overlay(&self) -> UserOverlay {
        UserOverlay {
            rules: if self.settings.custom_rules_enabled {
                self.rules.rules.clone()
            } else {
                Vec::new()
            },
            hosts: if self.settings.custom_hosts_enabled {
                self.hosts.entries.clone()
            } else {
                Vec::new()
            },
            ipv6: self.settings.ipv6,
            unified_delay: self.settings.unified_delay,
        }
    }

    /// Re-composes the running revision when the saved overlay differs from
    /// the one the core runs. Waits while a start or reload is in flight;
    /// those paths call back here when they finish. Returns whether a hot
    /// reload started now or will follow the one in flight.
    pub(super) fn sync_user_overlay(&mut self) -> bool {
        let wanted = self.user_overlay();
        let Some(running) = &self.running else {
            // A start in flight composes the saved overlay itself.
            return false;
        };
        if running.user == wanted || self.failed_overlay.as_ref() == Some(&wanted) {
            return false;
        }
        if self.view.runtime != RuntimeStatus::Running {
            return self.view.runtime.is_up();
        }
        let profile_id = running.profile_id.clone();
        let (Some(binary), Some(rev)) = (
            self.binary.clone(),
            self.profiles
                .get(&profile_id)
                .and_then(|p| p.active_revision.clone()),
        ) else {
            return false;
        };
        let Some(req) = self.reload_request(&profile_id, rev) else {
            return false;
        };
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        let previous = std::mem::replace(&mut running.user, req.user.clone());
        let attempted = req.user.clone();
        // The rules page already ran `-t` on exactly this overlay (§1.3).
        let checked = self.checked_overlay.take().is_some_and(|c| {
            c.profile_id == profile_id
                && c.revision == req.candidate
                && c.tun == req.tun
                && c.user == attempted
        });
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
                outcome: runtime::reapply(req, binary, previous, checked).await,
            }
        });
        true
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
        // Named after what changed, so a rules page save is not reported as settings.
        let label = changed_label(&attempted, &previous);
        let action = format!("应用{label}");
        let (reason, restart) = match outcome {
            OverlayOutcome::Applied => {
                tracing::info!("overlay reload applied");
                self.record_operation(
                    &action,
                    &name,
                    "手动",
                    "成功",
                    &format!("已热重载 · {}", attempted.describe()),
                );
                self.failed_overlay = None;
                self.view.notice = Some(info_notice(format!("{label}已热重载生效")));
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
        // Restore the settings this attempt changed unless the user already
        // changed them again. Saved override files stay; the core keeps
        // running without them.
        let settings_changed =
            (attempted.ipv6, attempted.unified_delay) != (previous.ipv6, previous.unified_delay);
        if settings_changed
            && self.settings.ipv6 == attempted.ipv6
            && self.settings.unified_delay == attempted.unified_delay
        {
            self.settings.ipv6 = previous.ipv6;
            self.settings.unified_delay = previous.unified_delay;
            if let Err(e) = self.save_settings_file() {
                tracing::warn!(error = %e, "overlay settings not restored");
            }
            self.refresh_settings_view();
            self.view.settings.revision += 1;
        }
        let restored = self.user_overlay() == previous;
        if !restored {
            // Not retried until the user changes something again; a newer
            // overlay saved meanwhile is tried by `finish_overlay_reload`.
            self.failed_overlay = Some(attempted);
        }
        let tail = match (restored, restart) {
            (true, false) => "已恢复原设置",
            (true, true) => "已恢复原设置，正在重启内核",
            (false, false) => "运行中的内核仍使用原 Hosts / 规则，请检查后重新保存",
            (false, true) => "已保存的 Hosts / 规则未生效，正在重启内核",
        };
        self.record_operation(&action, &name, "手动", "失败", &format!("{reason}；{tail}"));
        self.view.notice = Some(Notice::error(format!("{reason}；{tail}")));
        if restart && let Some(running) = self.running.take() {
            self.view.groups.clear();
            self.start_after(Some(running));
            return;
        }
        self.finish_overlay_reload();
    }

    /// Iteration 2 §1.3: a failed check or start may come from the user's
    /// rules or hosts; point the user at the rules page.
    pub(super) fn overlay_hint(&self) -> &'static str {
        let user = self.user_overlay();
        if user.rules.is_empty() && user.hosts.is_empty() {
            ""
        } else {
            "；可能由附加规则或 Hosts 导致，可在“规则”页停用后重试"
        }
    }

    fn finish_overlay_reload(&mut self) {
        if self.running.is_some() {
            self.restart_live_streams();
            self.refresh_groups();
        }
        self.commit_queued_revision();
    }
}

/// "设置" (IPv6 / unified delay), "附加规则", "Hosts", joined by what differs.
fn changed_label(attempted: &UserOverlay, previous: &UserOverlay) -> String {
    let mut parts = Vec::new();
    if (attempted.ipv6, attempted.unified_delay) != (previous.ipv6, previous.unified_delay) {
        parts.push("设置");
    }
    if attempted.rules != previous.rules {
        parts.push("附加规则");
    }
    if attempted.hosts != previous.hosts {
        parts.push("Hosts");
    }
    if parts.is_empty() {
        parts.push("设置");
    }
    parts.join("、")
}
