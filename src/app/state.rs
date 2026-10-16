//! Snapshot published to the UI (DESIGN.md §6.5 `Event::State`).

use crate::cores::mihomo::controller::ProxyGroupView;
use crate::domain::model::{AccountStatus, ProviderAccount};

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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub error: bool,
    pub text: String,
}

impl Notice {
    /// Error notice from already-composed, redacted text.
    pub fn error(text: String) -> Self {
        tracing::warn!(%text, "operation failed");
        Self { error: true, text }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AppStateView {
    pub core_version: String,
    pub core_ready: bool,
    pub runtime: RuntimeStatus,
    pub mixed_port: u16,
    /// User intent: point the system proxy at the core while connected.
    pub system_proxy_wanted: bool,
    /// The system proxy currently points at the core (journal says so).
    pub system_proxy_active: bool,
    pub connectivity: Connectivity,
    pub profiles: Vec<ProfileView>,
    pub selected_profile: String,
    pub groups: Vec<ProxyGroupView>,
    pub testing_group: Option<String>,
    pub busy: Option<String>,
    pub notice: Option<Notice>,
    pub policy_notes: Vec<String>,
    pub check_lines: Vec<String>,
    pub accounts: Vec<AccountView>,
    pub login_busy: bool,
    /// Last login failure, shown under the login form.
    pub login_message: Option<String>,
    pub probing_hosts: bool,
    /// Diagnostics: ziyoumao host discovery result lines.
    pub provider_hosts: Vec<String>,
}
