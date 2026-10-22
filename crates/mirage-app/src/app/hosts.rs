//! Hosts tab of the rules page (iteration 2 §2): validate → `-t` → save →
//! hot reload (§1.3).
use super::pipeline::{self, OverlayCheck};
use super::state::Notice;
use super::{AppCore, Internal, error_notice, info_notice};
use crate::domain::error::AppError;
use crate::overrides::hosts::{self, Hosts};

impl AppCore {
    pub(super) fn refresh_hosts_view(&mut self) {
        let saving = self.view.hosts.saving;
        self.view.hosts = super::state::HostsView {
            enabled: self.settings.custom_hosts_enabled,
            text: self.hosts.text.clone(),
            count: self.hosts.count(),
            errors: self.hosts.errors.clone(),
            saving,
        };
    }

    pub(super) fn save_hosts(&mut self, enabled: bool, text: String) {
        if self.view.hosts.saving {
            return;
        }
        let entries = match hosts::parse(&text) {
            Ok(entries) => entries,
            Err(errors) => {
                self.view.notice = Some(Notice::error(format!(
                    "Hosts 格式有误，未保存：{}",
                    errors.first().map(String::as_str).unwrap_or_default()
                )));
                self.view.hosts.errors = errors;
                return;
            }
        };
        self.view.hosts.errors.clear();
        let mut user = self.user_overlay();
        user.hosts = if enabled { entries } else { Vec::new() };
        // Only a change the core would see needs a kernel check.
        let check = (user != self.user_overlay())
            .then(|| self.overlay_check(user))
            .flatten();
        let Some(check) = check else {
            self.commit_hosts(enabled, text);
            return;
        };
        self.view.hosts.saving = true;
        self.spawn(async move {
            Internal::HostsChecked {
                enabled,
                text,
                result: pipeline::check_overlay(check).await,
            }
        });
    }

    pub(super) fn on_hosts_checked(
        &mut self,
        enabled: bool,
        text: String,
        result: Result<(), AppError>,
    ) {
        self.view.hosts.saving = false;
        match result {
            Ok(()) => self.commit_hosts(enabled, text),
            Err(e) => {
                let detail = e.user_message();
                self.record_operation("保存 Hosts", "", "手动", "失败", &detail);
                self.view.notice =
                    Some(error_notice(&e).with_stage("Hosts 未保存：当前配置的内核校验未通过"));
            }
        }
    }

    /// Writes the file and the switch, then hot-applies them if connected.
    fn commit_hosts(&mut self, enabled: bool, text: String) {
        let saved = Hosts::save(&self.layout, &text).and_then(|()| {
            let previous = self.settings.custom_hosts_enabled;
            self.settings.custom_hosts_enabled = enabled;
            self.save_settings_file().inspect_err(|_| {
                self.settings.custom_hosts_enabled = previous;
            })
        });
        if let Err(e) = saved {
            self.record_operation("保存 Hosts", "", "手动", "失败", &e.user_message());
            self.view.notice = Some(error_notice(&e));
            return;
        }
        let entries = hosts::parse(&text).unwrap_or_default();
        let count = entries.len();
        self.hosts = Hosts {
            text,
            entries,
            errors: Vec::new(),
        };
        self.refresh_hosts_view();
        self.record_operation(
            "保存 Hosts",
            "",
            "手动",
            "成功",
            &format!("{count} 条{}", if enabled { "" } else { "（未启用）" }),
        );
        let reload = self.view.runtime.is_up()
            && self
                .running
                .as_ref()
                .is_some_and(|r| r.user != self.user_overlay());
        self.view.notice = Some(info_notice(if reload {
            "Hosts 已保存，正在热重载内核"
        } else {
            "Hosts 已保存"
        }));
        self.sync_user_overlay();
    }

    /// Check against the selected profile's active revision, as the next
    /// start or reload would compose it. `None`: nothing to check yet.
    pub(super) fn overlay_check(
        &self,
        user: crate::cores::mihomo::compose::UserOverlay,
    ) -> Option<OverlayCheck> {
        let profile = self.profiles.selected()?.clone();
        Some(OverlayCheck {
            binary: self.binary.clone()?,
            layout: self.layout.clone(),
            revision: profile.active_revision.clone()?,
            profile,
            mixed_port: self.settings.mixed_port,
            tun: self.view.tun_wanted,
            user,
        })
    }
}
