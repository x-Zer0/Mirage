//! Settings page and per-profile options (DESIGN.md §17.1): ports, bypass,
//! default UA, provider egress, startup behaviour, routing mode, geodata.

use super::state::{Retry, RuntimeStatus, SettingsView};
use super::{AppCore, Command, Internal, error_notice, info_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{EgressPolicy, RoutingMode, SubscriptionSource};
use crate::platform::windows::clipboard;
use crate::storage::meta;

const STAGE: &str = "app.settings";
/// Ports below this are reserved or need elevation on some systems.
const MIN_PORT: u16 = 1024;
const MAX_BYPASS: usize = 4096;
const MAX_UA: usize = 256;
const MAX_INTERVAL_HOURS: u32 = 24 * 30;

/// Edits from the settings page; validated before anything is saved.
#[derive(Debug, Clone)]
pub struct SettingsInput {
    pub mixed_port: String,
    pub bypass: String,
    pub default_user_agent: String,
    /// Index into [`EgressPolicy::ALL`].
    pub provider_egress: usize,
    pub auto_connect: bool,
    pub close_to_tray: bool,
}

impl AppCore {
    pub(super) fn refresh_settings_view(&mut self) {
        let s = &self.settings;
        self.view.settings = SettingsView {
            mixed_port: s.mixed_port,
            bypass: s.bypass.clone(),
            default_user_agent: s.default_user_agent.clone(),
            provider_egress: EgressPolicy::ALL
                .iter()
                .position(|e| *e == s.provider_egress)
                .unwrap_or(0),
            auto_connect: s.auto_connect,
            close_to_tray: s.close_to_tray,
            close_hint_shown: s.close_hint_shown,
            core_path: self
                .layout
                .bundled_core
                .join("mihomo.exe")
                .display()
                .to_string(),
        };
        self.view.mixed_port = s.mixed_port;
    }

    pub(super) fn save_settings_file(&mut self) -> AppResult<()> {
        meta::save(&self.layout.settings_file(), &mut self.settings, STAGE)
    }

    pub(super) fn save_settings(&mut self, input: SettingsInput) {
        let parsed = match validate(&input) {
            Ok(p) => p,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        let port_changed = parsed.mixed_port != self.settings.mixed_port;
        let bypass_changed = parsed.bypass != self.settings.bypass;
        self.settings.mixed_port = parsed.mixed_port;
        self.settings.bypass = parsed.bypass;
        self.settings.default_user_agent = parsed.default_user_agent;
        self.settings.provider_egress = parsed.provider_egress;
        self.settings.auto_connect = input.auto_connect;
        self.settings.close_to_tray = input.close_to_tray;
        if let Err(e) = self.save_settings_file() {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.refresh_settings_view();
        let up = self.view.runtime.is_up();
        if port_changed && up {
            // §10.6: a new mixed-port needs a process restart. The system
            // proxy is withdrawn first and re-applied for the new port.
            self.release_sysproxy();
            if let Some(previous) = self.running.take() {
                self.view.groups.clear();
                self.start_after(Some(previous));
            }
            self.view.notice = Some(info_notice("设置已保存，正在用新端口重启内核"));
        } else if bypass_changed && self.view.system_proxy_active {
            self.release_sysproxy();
            self.ensure_sysproxy();
            self.view.notice = Some(info_notice("设置已保存，系统代理例外列表已更新"));
        } else {
            self.view.notice = Some(info_notice("设置已保存"));
        }
    }

    /// Records that the one-time close-to-tray hint was shown (§17.2).
    pub(super) fn close_hint_shown(&mut self) {
        if self.settings.close_hint_shown {
            return;
        }
        self.settings.close_hint_shown = true;
        if let Err(e) = self.save_settings_file() {
            tracing::warn!(error = %e, "settings not saved");
        }
        self.refresh_settings_view();
    }

    /// User-chosen mode: saved, applied at runtime via `PATCH /configs`, and
    /// written into every later composed config (§9.2).
    pub(super) fn set_routing_mode(&mut self, mode: RoutingMode) {
        self.settings.routing_mode = Some(mode);
        if let Err(e) = self.save_settings_file() {
            self.view.notice = Some(error_notice(&e));
        }
        self.view.routing_mode = Some(mode);
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        self.spawn(async move {
            let result = controller
                .patch_configs(serde_json::json!({ "mode": mode.as_str() }))
                .await;
            if result.is_ok() {
                // Rule/global switches keep connections on the old route.
                let _ = controller.close_all_connections().await;
            }
            Internal::ModeApplied(generation, result)
        });
    }

    pub(super) fn on_mode_applied(&mut self, generation: u64, result: AppResult<()>) {
        if generation != self.generation {
            return;
        }
        match result {
            Ok(()) => {
                if let Some(mode) = self.settings.routing_mode {
                    self.view.notice = Some(info_notice(format!("已切换为{}模式", mode.label())));
                }
                self.refresh_groups();
            }
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
    }

    /// Mode as the running core reports it, when the user never picked one.
    pub(super) fn read_core_mode(&self) {
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        self.spawn(async move { Internal::CoreMode(generation, controller.mode().await) });
    }

    pub(super) fn on_core_mode(&mut self, generation: u64, result: AppResult<String>) {
        if generation != self.generation || self.settings.routing_mode.is_some() {
            return;
        }
        match result {
            Ok(mode) => self.view.routing_mode = RoutingMode::parse(&mode),
            Err(e) => tracing::debug!(error = %e, "mode not readable"),
        }
    }

    pub(super) fn set_update_policy(&mut self, id: &str, interval_hours: u32, egress: usize) {
        let Some(profile) = self.profiles.get_mut(id) else {
            return;
        };
        let egress = EgressPolicy::ALL.get(egress).copied().unwrap_or_default();
        let interval_hours = interval_hours.min(MAX_INTERVAL_HOURS);
        let policy = &mut profile.update_policy;
        if policy.interval_hours != interval_hours {
            policy.interval_hours = interval_hours;
            policy.user_set = true;
        }
        policy.egress = egress;
        self.save_profiles();
        self.refresh_profiles_view();
        self.view.notice = Some(info_notice("更新设置已保存"));
    }

    /// The only path a full subscription URL takes out of the SecretStore
    /// (§17.3); it is never put into a UI property.
    pub(super) fn copy_profile_url(&mut self, id: &str) {
        let Some(SubscriptionSource::RemoteUrl { url_secret, .. }) =
            self.profiles.get(id).map(|p| p.source.clone())
        else {
            return;
        };
        let copied = self
            .secrets
            .get(&url_secret)
            .and_then(|v| {
                v.ok_or_else(|| {
                    AppError::new(AppErrorKind::Storage, STAGE, "找不到已保存的订阅地址")
                })
            })
            .and_then(|bytes| {
                let text = zeroize::Zeroizing::new(String::from_utf8_lossy(&bytes).into_owned());
                clipboard::set_text(&text).map_err(|e| AppError::internal(STAGE, e))
            });
        self.view.notice = Some(match copied {
            Ok(()) => info_notice("订阅链接已复制到剪贴板"),
            Err(e) => error_notice(&e),
        });
    }

    pub(super) fn update_geodata(&mut self) {
        if self.view.geo_updating {
            return;
        }
        let Some(running) = self.running.as_ref().filter(|_| self.view.runtime.is_up()) else {
            self.view.notice = Some(info_notice("请先连接，geodata 通过运行中的内核更新"));
            return;
        };
        let controller = running.controller.clone();
        self.view.geo_updating = true;
        self.spawn(async move { Internal::GeoUpdated(controller.upgrade_geo().await) });
    }

    pub(super) fn on_geo_updated(&mut self, result: AppResult<()>) {
        self.view.geo_updating = false;
        self.view.notice = Some(match result {
            Ok(()) => info_notice("geodata 已更新"),
            Err(e) => error_notice(&e).with_stage("geodata 更新"),
        });
    }

    /// Error bar retry button (§17.3).
    pub(super) fn retry(&mut self) {
        let Some(retry) = self.view.error.take().and_then(|n| n.retry) else {
            return;
        };
        let cmd = match retry {
            Retry::Connect => {
                if self.view.runtime != RuntimeStatus::Stopped
                    && !matches!(self.view.runtime, RuntimeStatus::Failed(_))
                {
                    return;
                }
                Command::Connect
            }
            Retry::UpdateProfile(id) => Command::UpdateProfile(id),
            Retry::RevalidateAccount(id) => Command::RevalidateAccount(id),
        };
        self.handle(cmd);
    }
}

struct Parsed {
    mixed_port: u16,
    bypass: String,
    default_user_agent: String,
    provider_egress: EgressPolicy,
}

fn validate(input: &SettingsInput) -> AppResult<Parsed> {
    let invalid = |detail: &str| AppError::new(AppErrorKind::InvalidInput, STAGE, detail);
    let mixed_port: u16 = input
        .mixed_port
        .trim()
        .parse()
        .ok()
        .filter(|p| *p >= MIN_PORT)
        .ok_or_else(|| invalid(&format!("端口必须是 {MIN_PORT}–65535 之间的数字")))?;
    let bypass: String = input
        .bypass
        .split([';', '\n', '\r', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(";");
    if bypass.len() > MAX_BYPASS || bypass.chars().any(char::is_control) {
        return Err(invalid("例外列表过长或包含非法字符"));
    }
    let ua = input.default_user_agent.trim();
    if ua.is_empty() || ua.len() > MAX_UA || !ua.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return Err(invalid("User-Agent 不能为空，且只能包含 ASCII 可见字符"));
    }
    Ok(Parsed {
        mixed_port,
        bypass,
        default_user_agent: ua.to_owned(),
        provider_egress: EgressPolicy::ALL
            .get(input.provider_egress)
            .copied()
            .unwrap_or_default(),
    })
}
