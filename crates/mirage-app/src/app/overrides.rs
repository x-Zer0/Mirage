//! Rules page (iteration 2 §2, §3): extra rules and hosts share one save
//! flow — validate → `-t` on the active config → save → hot reload (§1.3).
use super::pipeline::{self, OverlayCheck};
use super::runtime::CheckedOverlay;
use super::state::{Notice, OverrideView};
use super::{AppCore, Internal, error_notice, info_notice};
use crate::cores::mihomo::compose::UserOverlay;
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::overrides::hosts::{self, Hosts};
use crate::overrides::rules::{self, Rules};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideKind {
    Rules,
    Hosts,
}

impl OverrideKind {
    fn label(self) -> &'static str {
        match self {
            Self::Rules => "附加规则",
            Self::Hosts => "Hosts",
        }
    }
}

/// Overlay notes from the kernel check (skipped rules etc.).
pub type CheckResult = Result<Vec<String>, AppError>;

impl AppCore {
    pub(super) fn refresh_override_views(&mut self) {
        fn view(
            enabled: bool,
            text: &str,
            count: usize,
            errors: &[String],
            old: &OverrideView,
        ) -> OverrideView {
            OverrideView {
                enabled,
                text: text.to_owned(),
                count,
                errors: errors.to_vec(),
                notes: old.notes.clone(),
                saving: old.saving,
            }
        }
        self.view.rules = view(
            self.settings.custom_rules_enabled,
            &self.rules.text,
            self.rules.rules.len(),
            &self.rules.errors,
            &self.view.rules,
        );
        self.view.hosts = view(
            self.settings.custom_hosts_enabled,
            &self.hosts.text,
            self.hosts.entries.len(),
            &self.hosts.errors,
            &self.view.hosts,
        );
    }

    fn override_view(&mut self, kind: OverrideKind) -> &mut OverrideView {
        match kind {
            OverrideKind::Rules => &mut self.view.rules,
            OverrideKind::Hosts => &mut self.view.hosts,
        }
    }

    pub(super) fn save_override(&mut self, kind: OverrideKind, enabled: bool, text: String) {
        if self.override_view(kind).saving {
            return;
        }
        let user = match self.override_candidate(kind, enabled, &text) {
            Ok(user) => user,
            Err(errors) => {
                self.view.notice = Some(Notice::error(format!(
                    "{}格式有误，未保存：{}",
                    kind.label(),
                    errors.first().map(String::as_str).unwrap_or_default()
                )));
                self.override_view(kind).errors = errors;
                return;
            }
        };
        self.override_view(kind).errors.clear();
        // Only a change the core would see needs a kernel check.
        let check = (user != self.user_overlay())
            .then(|| self.overlay_check(user))
            .flatten();
        let Some(check) = check else {
            self.commit_override(kind, enabled, text, None);
            return;
        };
        self.override_view(kind).saving = true;
        let checked = CheckedOverlay {
            profile_id: check.profile.id.clone(),
            revision: check.revision.clone(),
            tun: check.tun,
            user: check.user.clone(),
        };
        self.spawn(async move {
            Internal::OverrideChecked {
                kind,
                enabled,
                text,
                checked,
                result: pipeline::check_overlay(check).await,
            }
        });
    }

    /// The overlay the core would run with this text saved.
    fn override_candidate(
        &self,
        kind: OverrideKind,
        enabled: bool,
        text: &str,
    ) -> Result<UserOverlay, Vec<String>> {
        let mut user = self.user_overlay();
        match kind {
            OverrideKind::Rules => {
                let parsed = rules::parse_yaml(text)?;
                user.rules = if enabled { parsed } else { Vec::new() };
            }
            OverrideKind::Hosts => {
                let parsed = hosts::parse(text)?;
                user.hosts = if enabled { parsed } else { Vec::new() };
            }
        }
        Ok(user)
    }

    pub(super) fn on_override_checked(
        &mut self,
        kind: OverrideKind,
        enabled: bool,
        text: String,
        checked: CheckedOverlay,
        result: CheckResult,
    ) {
        self.override_view(kind).saving = false;
        match result {
            Ok(notes) => {
                self.checked_overlay = Some(checked);
                self.commit_override(kind, enabled, text, Some(notes));
            }
            Err(e) => {
                let action = format!("保存{}", kind.label());
                self.record_operation(&action, "", "手动", "失败", &e.user_message());
                // The check can also fail before the kernel runs (reading
                // the revision, composing, generating the secret).
                let reason = match e.kind {
                    AppErrorKind::ConfigCheckFailed => "当前配置的内核校验未通过",
                    AppErrorKind::PolicyRejected | AppErrorKind::NotAConfig => {
                        "当前配置无法合成运行配置"
                    }
                    _ => "无法完成内核校验",
                };
                self.view.notice =
                    Some(error_notice(&e).with_stage(&format!("{}未保存：{reason}", kind.label())));
            }
        }
    }

    /// Writes the file and the switch, then hot-applies them if connected.
    /// `notes`: the overlay's notes from the check, when one ran.
    fn commit_override(
        &mut self,
        kind: OverrideKind,
        enabled: bool,
        text: String,
        notes: Option<Vec<String>>,
    ) {
        let action = format!("保存{}", kind.label());
        if let Err(e) = self.store_override(kind, enabled, &text) {
            self.record_operation(&action, "", "手动", "失败", &e.user_message());
            self.view.notice = Some(error_notice(&e));
            return;
        }
        let count = match kind {
            OverrideKind::Rules => {
                let parsed = rules::parse_yaml(&text).unwrap_or_default();
                let count = parsed.len();
                self.rules = Rules {
                    text,
                    rules: parsed,
                    errors: Vec::new(),
                };
                count
            }
            OverrideKind::Hosts => {
                let entries = hosts::parse(&text).unwrap_or_default();
                let count = entries.len();
                self.hosts = Hosts {
                    text,
                    entries,
                    errors: Vec::new(),
                };
                count
            }
        };
        // Skipped-rule notes concern the rules tab only.
        if kind == OverrideKind::Rules
            && let Some(notes) = &notes
        {
            self.view.rules.notes = notes
                .iter()
                .filter(|n| n.starts_with("跳过") || n.contains("find-process-mode"))
                .cloned()
                .collect();
        }
        self.refresh_override_views();
        self.record_operation(
            &action,
            "",
            "手动",
            "成功",
            &format!("{count} 条{}", if enabled { "" } else { "（未启用）" }),
        );
        let reload = self.sync_user_overlay();
        // Only the reload this save started may skip its check.
        self.checked_overlay = None;
        let mut message = format!(
            "{}已保存{}",
            kind.label(),
            if reload {
                "，正在热重载内核"
            } else {
                ""
            }
        );
        if kind == OverrideKind::Rules
            && let Some(first) = self.view.rules.notes.first()
        {
            message = format!("{message}；{first}");
        }
        self.view.notice = Some(info_notice(message));
    }

    fn store_override(&mut self, kind: OverrideKind, enabled: bool, text: &str) -> AppResult<()> {
        match kind {
            OverrideKind::Rules => Rules::save(&self.layout, text)?,
            OverrideKind::Hosts => Hosts::save(&self.layout, text)?,
        }
        let flag = match kind {
            OverrideKind::Rules => &mut self.settings.custom_rules_enabled,
            OverrideKind::Hosts => &mut self.settings.custom_hosts_enabled,
        };
        let previous = std::mem::replace(flag, enabled);
        let saved = self.save_settings_file();
        if saved.is_err() {
            match kind {
                OverrideKind::Rules => self.settings.custom_rules_enabled = previous,
                OverrideKind::Hosts => self.settings.custom_hosts_enabled = previous,
            }
        }
        saved
    }

    /// Check against the selected profile's active revision, as the next
    /// start or reload would compose it. `None`: nothing to check yet.
    pub(super) fn overlay_check(&self, user: UserOverlay) -> Option<OverlayCheck> {
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
