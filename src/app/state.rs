//! Snapshot published to the UI (DESIGN.md §6.5 `Event::State`).

use crate::cores::mihomo::controller::ProxyGroupView;
use crate::domain::model::{AccountStatus, ProviderAccount, RoutingMode};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RuntimeStatus {
    #[default]
    Stopped,
    Starting,
    Running,
    /// Running; a new revision is being hot-applied (§5.2).
    Reloading,
    /// Unexpected exit; a supervised restart is pending (§5.2).
    Restarting,
    Stopping,
    Failed(String),
}

impl RuntimeStatus {
    /// The core is up and serving the mixed port.
    pub fn is_up(&self) -> bool {
        matches!(self, Self::Running | Self::Reloading)
    }
}

/// Reachability through the local core, independent of the core state (§5.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Connectivity {
    #[default]
    Unknown,
    Checking,
    Reachable(u32),
    Unreachable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileView {
    pub id: String,
    pub name: String,
    pub source: String,
    pub detail: String,
    pub updated: String,
    pub selected: bool,
    pub remote: bool,
    pub updating: bool,
    /// "已用 1.2 GiB / 100 GiB · 到期 2026-12-01"; empty when unknown.
    pub usage: String,
    /// Last fetch failure ("更新失败（仍使用 … 的版本）：…"); empty when fine.
    pub fetch_error: String,
    /// The subscription URL can be copied (plain URL profiles only).
    pub has_url: bool,
    /// 0 = manual only.
    pub interval_hours: u32,
    /// Index into [`EgressPolicy::ALL`].
    pub egress: usize,
    /// Composer adjustments of the active revision (§9.5).
    pub policy_notes: Vec<String>,
}

/// One provider account as shown on the accounts page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    pub id: String,
    pub name: String,
    pub status: String,
    /// 0 neutral, 1 ok, 2 error.
    pub status_level: i32,
    pub busy: bool,
    pub plan: String,
    pub usage: String,
    pub expires: String,
    pub has_profile: bool,
    pub profile_id: String,
}

impl AccountView {
    pub fn from_account(a: &ProviderAccount) -> Self {
        let (status, status_level) = match &a.status {
            AccountStatus::LoggedOut => ("未登录".to_owned(), 0),
            AccountStatus::LoggingIn => ("正在登录…".to_owned(), 0),
            AccountStatus::Validating => ("正在验证会话…".to_owned(), 0),
            AccountStatus::Valid => ("已登录".to_owned(), 1),
            AccountStatus::ReauthRequired => ("登录已失效，请重新登录".to_owned(), 2),
            AccountStatus::NeedsUserAction(t) => (t.clone(), 2),
            AccountStatus::NetworkUnavailable(t) => (format!("暂时无法连接服务：{t}"), 2),
        };
        let snap = a.snapshot.clone().unwrap_or_default();
        let gib = |b: u64| format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0));
        let usage = match (snap.used_bytes, snap.total_bytes) {
            (None, None) => String::new(),
            (used, total) => format!(
                "已用 {} / {}",
                used.map_or_else(|| "未提供".into(), gib),
                total.map_or_else(|| "未提供".into(), gib)
            ),
        };
        let expires = snap
            .expires_at
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_else(|| "未提供".into());
        let mut plan = snap.plan_name.unwrap_or_default();
        if snap.banned == Some(true) {
            plan.push_str("（账号已被停用）");
        }
        Self {
            id: a.id.clone(),
            name: a.display_name.clone(),
            status,
            status_level,
            busy: a.status == AccountStatus::Validating,
            plan,
            usage,
            expires: format!("到期 {expires}"),
            has_profile: a.profile_id.is_some(),
            profile_id: a.profile_id.clone().unwrap_or_default(),
        }
    }
}

/// What the error bar's retry button re-sends (§17.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retry {
    Connect,
    UpdateProfile(String),
    RevalidateAccount(String),
}

/// One user-facing message. Info notices are shown as a toast that fades out;
/// errors stay in the error bar until dismissed or replaced (§17.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Increases with every notice, so the UI can tell a repeat from a new one.
    pub seq: u64,
    pub error: bool,
    /// Step that failed, in user terms ("订阅下载"); empty when unknown.
    pub stage: String,
    pub text: String,
    pub retry: Option<Retry>,
    /// Also raise a tray notification (for events the user must see even
    /// with the window hidden, §11.4).
    pub notify: bool,
}

static NOTICE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Notice {
    fn make(error: bool, stage: String, text: String) -> Self {
        Self {
            seq: NOTICE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            error,
            stage,
            text,
            retry: None,
            notify: false,
        }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self::make(false, String::new(), text.into())
    }

    /// Error notice from already-composed, redacted text.
    pub fn error(text: String) -> Self {
        tracing::warn!(%text, "operation failed");
        Self::make(true, String::new(), text)
    }

    pub fn from_error(e: &crate::domain::error::AppError) -> Self {
        tracing::warn!(error = %e, "operation failed");
        Self::make(true, stage_label(e.stage).into(), e.user_message())
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = Some(retry);
        self
    }

    pub fn with_stage(mut self, stage: &str) -> Self {
        self.stage = stage.into();
        self
    }

    pub fn notifying(mut self) -> Self {
        self.notify = true;
        self
    }
}

/// Maps an internal error stage to the step name shown in the error bar;
/// module names never reach the UI (§17.3).
pub fn stage_label(stage: &str) -> &'static str {
    match stage {
        "app.runtime" | "mihomo.process" => "内核启动",
        "mihomo.binary" => "内核校验",
        "mihomo.check" | "mihomo.compose" | "subscriptions.detect" => "配置校验",
        "mihomo.controller" => "内核控制",
        "subscriptions.fetch" | "net" => "订阅下载",
        "subscriptions.import" => "导入文件",
        "subscriptions.revisions" | "storage.journal" | "app.profiles" => "保存数据",
        "ziyoumao" | "app.accounts" => "自由猫账号",
        "app.sysproxy" => "系统代理",
        "app.supervise" => "内核监督",
        "app.connectivity" => "连通检测",
        "app.updates" => "订阅更新",
        "platform.secret" => "凭据存储",
        "app.settings" => "设置",
        _ => "",
    }
}

/// Settings as shown on the settings page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsView {
    pub mixed_port: u16,
    pub bypass: String,
    pub default_user_agent: String,
    /// Index into [`EgressPolicy::ALL`].
    pub provider_egress: usize,
    pub auto_connect: bool,
    pub close_to_tray: bool,
    pub close_hint_shown: bool,
    /// Start with Windows (§11.6); reflects the saved preference.
    pub autostart: bool,
    pub tun_disable_system_proxy: bool,
    pub core_path: String,
}

/// Bytes per second from `/traffic` (§6.5 `Event::Traffic`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrafficSample {
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppStateView {
    pub core_version: String,
    pub core_ready: bool,
    pub runtime: RuntimeStatus,
    pub mixed_port: u16,
    /// User intent: point the system proxy at the core while connected.
    pub system_proxy_wanted: bool,
    /// The system proxy currently points at the core (journal says so).
    pub system_proxy_active: bool,
    pub tun_wanted: bool,
    pub tun_active: bool,
    pub service_installed: bool,
    pub service_busy: bool,
    pub connectivity: Connectivity,
    /// Effective mode: the user's choice, else what the running core reports.
    pub routing_mode: Option<RoutingMode>,
    pub profiles: Vec<ProfileView>,
    pub selected_profile: String,
    pub groups: Vec<ProxyGroupView>,
    pub testing_group: Option<String>,
    pub busy: Option<String>,
    /// Set by handlers; moved into `error` or `toast` before publishing.
    pub notice: Option<Notice>,
    /// Error bar: stays until dismissed, retried or replaced by another error.
    pub error: Option<Notice>,
    /// Latest info message; the UI shows it briefly.
    pub toast: Option<Notice>,
    pub policy_notes: Vec<String>,
    pub check_lines: Vec<String>,
    pub accounts: Vec<AccountView>,
    pub login_busy: bool,
    /// Last login failure, shown under the login form.
    pub login_message: Option<String>,
    pub probing_hosts: bool,
    /// Diagnostics: ziyoumao host discovery result lines.
    pub provider_hosts: Vec<String>,
    pub settings: SettingsView,
    pub geo_updating: bool,
    /// A diagnostics bundle is being collected or saved (§18).
    pub exporting: bool,
}
