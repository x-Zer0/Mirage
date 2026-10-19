//! Settings page and per-profile options (DESIGN.md §17.1): ports, bypass,
//! default UA, provider egress, startup behaviour, routing mode, geodata.

use super::state::{Retry, RuntimeStatus, SettingsView};
use super::{AppCore, Command, Internal, error_notice, info_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{EgressPolicy, RoutingMode, SubscriptionSource};
use crate::platform::windows::autostart;
use crate::platform::windows::clipboard;
use crate::storage::meta;

const STAGE: &str = "app.settings";
/// Ports below this are reserved or need elevation on some systems.
const MIN_PORT: u16 = 1024;
const MAX_BYPASS: usize = 4096;
const MAX_UA: usize = 256;
const MAX_INTERVAL_HOURS: u32 = 24 * 30;
const MAX_CONNECTION_LOG_LIMIT: usize = 10_000;
const MAX_OPERATION_LOG_DAYS: usize = 60;

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
    /// Start with Windows (§11.6); applied to the Run key, not to the config.
    pub autostart: bool,
    pub tun_disable_system_proxy: bool,
    pub connection_log_limit: String,
    pub operation_log_days: String,
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
            autostart: s.autostart,
            tun_disable_system_proxy: s.tun_disable_system_proxy,
            connection_log_limit: s.connection_log_limit,
            operation_log_days: s.operation_log_days,
            core_path: self
                .layout
                .bundled_core
                .join("mihomo.exe")
                .display()
                .to_string(),
        };
        self.view.mixed_port = s.mixed_port;
    }

    pub(super) fn set_ui_geometry(&mut self, geometry: meta::UiGeometry) {
        let geometry = geometry.normalized();
        if self.settings.ui_geometry != geometry {
            self.settings.ui_geometry = geometry;
            self.ui_geometry_dirty = true;
        }
    }

    pub(super) fn flush_ui_geometry(&mut self) {
        if !self.ui_geometry_dirty {
            return;
        }
        match self.save_settings_file() {
            Ok(()) => self.ui_geometry_dirty = false,
            Err(e) => tracing::warn!(error = %e, "save UI geometry failed"),
        }
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
        let changes = setting_changes(&self.settings, &parsed, &input);
        let port_changed = parsed.mixed_port != self.settings.mixed_port;
        let bypass_changed = parsed.bypass != self.settings.bypass;
        let autostart_changed = input.autostart != self.settings.autostart;

        // Snapshot before mutation
        let snapshot = self.settings.clone();

        self.settings.mixed_port = parsed.mixed_port;
        self.settings.bypass = parsed.bypass;
        self.settings.default_user_agent = parsed.default_user_agent;
        self.settings.provider_egress = parsed.provider_egress;
        self.settings.auto_connect = input.auto_connect;
        self.settings.close_to_tray = input.close_to_tray;
        self.settings.autostart = input.autostart;
        let tun_proxy_changed =
            self.settings.tun_disable_system_proxy != input.tun_disable_system_proxy;
        self.settings.tun_disable_system_proxy = input.tun_disable_system_proxy;
        self.settings.connection_log_limit = parsed.connection_log_limit;
        self.settings.operation_log_days = parsed.operation_log_days;

        if let Err(e) = self.save_settings_file() {
            // Rollback on save failure
            self.settings = snapshot;
            self.record_operation("保存设置", "", "手动", "失败", &e.user_message());
            self.view.notice = Some(error_notice(&e));
            return;
        }

        // Only execute side effects after successful save
        self.connection_logs.set_limit(parsed.connection_log_limit);
        if parsed.operation_log_days == 0 {
            let _ = self.operation_logs.clear(&self.layout);
        } else {
            let _ = self
                .operation_logs
                .prune(&self.layout, parsed.operation_log_days);
        }

        if !changes.is_empty() {
            self.record_operation("保存设置", "", "手动", "成功", &changes.join("；"));
        }
        self.refresh_settings_view();
        if tun_proxy_changed && self.view.tun_active {
            self.view.system_proxy_wanted = !input.tun_disable_system_proxy
                && self.proxy_before_tun.unwrap_or(self.settings.system_proxy);
            if !self.view.system_proxy_wanted {
                self.release_sysproxy();
            }
        }
        if self.view.runtime.is_up() {
            self.ensure_sysproxy();
        }
        if autostart_changed {
            self.apply_autostart(input.autostart);
        }
        // The one notice that is shown: a restart, the bypass list, the
        // autostart entry, or a plain confirmation. The earlier branches win
        // because they describe work the user has to wait for.
        let up = self.view.runtime.is_up();
        if port_changed && up {
            // §10.6: a new mixed-port needs a process restart. The system
            // proxy is withdrawn first and re-applied for the new port.
            self.release_sysproxy();
            if let Some(previous) = self.running.take() {
                self.view.groups.clear();
                self.connection_source = "修改端口";
                self.start_after(Some(previous));
            }
            self.view.notice = Some(info_notice("设置已保存，正在用新端口重启内核"));
        } else if bypass_changed && self.view.system_proxy_active {
            self.release_sysproxy();
            self.ensure_sysproxy();
            self.view.notice = Some(info_notice("设置已保存，系统代理例外列表已更新"));
        } else if !autostart_changed {
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

    /// Writes or removes the Run entry (§11.6). The saved preference is the
    /// source of truth, so the registry is repaired whenever it disagrees.
    pub(super) fn apply_autostart(&mut self, wanted: bool) {
        let result = autostart::apply(wanted);
        if let Err(e) = &result {
            self.record_operation("设置开机启动", "", "手动", "失败", &e.to_string());
        }
        if let Err(e) = result.as_ref() {
            tracing::warn!(error = %e, "autostart entry not updated");
        }
        self.view.notice = Some(match result {
            Ok(true) if wanted => info_notice("已设置为开机启动"),
            Ok(true) => info_notice("已取消开机启动"),
            Ok(false) => info_notice("开机启动设置无需更改"),
            Err(e) => error_notice(&AppError::internal(STAGE, e)),
        });
    }

    /// User-chosen mode: saved, applied at runtime via `PATCH /configs`, and
    /// written into every later composed config (§9.2).
    pub(super) fn set_routing_mode(&mut self, mode: RoutingMode) {
        if self.view.routing_mode == Some(mode) {
            return;
        }
        self.settings.routing_mode = Some(mode);
        if let Err(e) = self.save_settings_file() {
            self.record_operation(
                "修改路由模式",
                mode.label(),
                "手动",
                "失败",
                &e.user_message(),
            );
            self.view.notice = Some(error_notice(&e));
            return;
        }
        let Some(running) = &self.running else {
            self.view.routing_mode = Some(mode);
            self.record_operation(
                "修改路由模式",
                mode.label(),
                "手动",
                "成功",
                "设置已保存，下次连接生效",
            );
            return;
        };
        let controller = running.controller.clone();
        let generation = self.generation;
        self.spawn(async move {
            let result = controller
                .patch_configs(serde_json::json!({ "mode": mode.as_str() }))
                .await;
            if result.is_ok() {
                let _ = controller.close_all_connections().await;
            }
            Internal::ModeApplied(generation, mode, result)
        });
    }
    pub(super) fn on_mode_applied(
        &mut self,
        generation: u64,
        mode: RoutingMode,
        result: AppResult<()>,
    ) {
        if generation != self.generation {
            return;
        }
        self.record_operation(
            "修改路由模式",
            mode.label(),
            "手动",
            if result.is_ok() { "成功" } else { "失败" },
            &result
                .as_ref()
                .map(|_| "已生效".to_owned())
                .unwrap_or_else(|e| e.user_message()),
        );
        match result {
            Ok(()) => {
                self.view.routing_mode = Some(mode);
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
        if profile.update_policy.interval_hours == interval_hours
            && profile.update_policy.egress == egress
        {
            return;
        }
        let name = profile.name.clone();
        let summary = format!(
            "更新间隔：{} → {} 小时（0 为手动）；出口：{} → {}",
            profile.update_policy.interval_hours,
            interval_hours,
            profile.update_policy.egress.label(),
            egress.label()
        );
        let policy = &mut profile.update_policy;
        if policy.interval_hours != interval_hours {
            policy.interval_hours = interval_hours;
            policy.user_set = true;
        }
        policy.egress = egress;
        if !self.save_profiles() {
            self.record_operation("修改更新策略", &name, "手动", "失败", "配置保存失败");
            return;
        }
        self.record_operation("修改更新策略", &name, "手动", "成功", &summary);
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
        self.record_operation(
            "更新规则数据",
            "geodata",
            "手动",
            if result.is_ok() { "成功" } else { "失败" },
            &result
                .as_ref()
                .map(|_| "规则数据已更新".to_owned())
                .unwrap_or_else(|e| e.user_message()),
        );
        self.view.notice = Some(match result {
            Ok(()) => info_notice("geodata 已更新"),
            Err(e) => error_notice(&e).with_stage("geodata 更新"),
        });
    }

    /// Autostart is applied from [`Self::save_settings`] when the switch
    /// changes, and reconciled at startup (§11.6).
    ///
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
    connection_log_limit: usize,
    operation_log_days: usize,
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
    let connection_log_limit = parse_range(
        &input.connection_log_limit,
        MAX_CONNECTION_LOG_LIMIT,
        "连接日志条数",
    )?;
    let operation_log_days = parse_range(
        &input.operation_log_days,
        MAX_OPERATION_LOG_DAYS,
        "配置日志保留天数",
    )?;
    let ua = input.default_user_agent.trim();
    if ua.is_empty() || ua.len() > MAX_UA || !ua.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return Err(invalid("User-Agent 不能为空，且只能包含 ASCII 可见字符"));
    }
    Ok(Parsed {
        mixed_port,
        bypass,
        default_user_agent: ua.to_owned(),
        connection_log_limit,
        operation_log_days,
        provider_egress: EgressPolicy::ALL
            .get(input.provider_egress)
            .copied()
            .unwrap_or_default(),
    })
}

fn parse_range(value: &str, max: usize, label: &str) -> AppResult<usize> {
    value
        .trim()
        .parse()
        .ok()
        .filter(|v| *v <= max)
        .ok_or_else(|| {
            AppError::new(
                AppErrorKind::InvalidInput,
                STAGE,
                format!("{label}必须是 0–{max} 之间的整数"),
            )
        })
}

fn setting_changes(
    saved: &meta::SettingsFile,
    parsed: &Parsed,
    input: &SettingsInput,
) -> Vec<String> {
    let mut changes = Vec::new();
    if parsed.mixed_port != saved.mixed_port {
        changes.push(format!("端口 {} → {}", saved.mixed_port, parsed.mixed_port));
    }
    if parsed.bypass != saved.bypass {
        changes.push("系统代理例外列表已修改".into());
    }
    if parsed.default_user_agent != saved.default_user_agent {
        changes.push("订阅 User-Agent 已修改".into());
    }
    if parsed.connection_log_limit != saved.connection_log_limit {
        changes.push(format!(
            "连接日志条数：{} → {}",
            saved.connection_log_limit, parsed.connection_log_limit
        ));
    }
    if parsed.operation_log_days != saved.operation_log_days {
        changes.push(format!(
            "配置日志保留天数：{} → {}",
            saved.operation_log_days, parsed.operation_log_days
        ));
    }
    if parsed.provider_egress != saved.provider_egress {
        changes.push(format!(
            "账号更新出口：{} → {}",
            saved.provider_egress.label(),
            parsed.provider_egress.label()
        ));
    }
    for (label, before, after) in [
        ("自动连接", saved.auto_connect, input.auto_connect),
        ("关闭到托盘", saved.close_to_tray, input.close_to_tray),
        ("开机启动", saved.autostart, input.autostart),
        (
            "TUN 自动关闭系统代理",
            saved.tun_disable_system_proxy,
            input.tun_disable_system_proxy,
        ),
    ] {
        if before != after {
            changes.push(format!(
                "{label}：{} → {}",
                if before { "开" } else { "关" },
                if after { "开" } else { "关" }
            ));
        }
    }
    changes
}
