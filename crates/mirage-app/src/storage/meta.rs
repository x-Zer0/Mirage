use super::atomic;
use crate::domain::error::{AppError, AppResult};
use crate::domain::model::{
    ConfigRevision, DEFAULT_SUBSCRIPTION_UA, EgressPolicy, Profile, ProfileId, ProviderAccount,
    RoutingMode,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilesFile {
    pub selected: Option<ProfileId>,
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionsFile {
    pub revisions: Vec<ConfigRevision>,
}

/// `accounts.json` (DESIGN.md §13): provider accounts, SecretRefs only.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountsFile {
    pub accounts: Vec<ProviderAccount>,
}

/// `{group: node}` remembered per profile (DESIGN.md §10.6).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionFile {
    pub groups: BTreeMap<String, String>,
}

/// UI dimensions in logical pixels; restored before the first window is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiGeometry {
    pub window_width: u32,
    pub window_height: u32,
    pub sidebar_width: u32,
}

impl Default for UiGeometry {
    fn default() -> Self {
        Self {
            window_width: 900,
            window_height: 680,
            sidebar_width: 150,
        }
    }
}

impl UiGeometry {
    pub fn normalized(self) -> Self {
        Self {
            window_width: self.window_width.clamp(700, 16384),
            window_height: self.window_height.clamp(460, 16384),
            sidebar_width: self.sidebar_width.clamp(64, 200),
        }
    }
}

/// User preferences (DESIGN.md §13 `settings.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsFile {
    #[serde(default)]
    pub ui_geometry: UiGeometry,
    #[serde(default = "default_true")]
    pub system_proxy: bool,
    #[serde(default)]
    pub tun_disable_system_proxy: bool,
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default)]
    pub resume_connected: bool,
    #[serde(default)]
    pub tun_enabled: bool,
    #[serde(default = "default_mixed_port")]
    pub mixed_port: u16,
    #[serde(default = "default_bypass")]
    pub bypass: String,
    #[serde(default = "default_ua")]
    pub default_user_agent: String,
    #[serde(default)]
    pub provider_egress: EgressPolicy,
    #[serde(default)]
    pub routing_mode: Option<RoutingMode>,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub close_hint_shown: bool,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_connection_log_limit")]
    pub connection_log_limit: usize,
    #[serde(default = "default_operation_log_days")]
    pub operation_log_days: usize,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            ui_geometry: UiGeometry::default(),
            system_proxy: true,
            tun_disable_system_proxy: false,
            auto_connect: false,
            resume_connected: false,
            tun_enabled: false,
            mixed_port: default_mixed_port(),
            bypass: default_bypass(),
            default_user_agent: default_ua(),
            provider_egress: EgressPolicy::default(),
            routing_mode: None,
            close_to_tray: true,
            close_hint_shown: false,
            autostart: false,
            connection_log_limit: default_connection_log_limit(),
            operation_log_days: default_operation_log_days(),
        }
    }
}

fn default_mixed_port() -> u16 {
    crate::app::DEFAULT_MIXED_PORT
}

fn default_bypass() -> String {
    crate::app::sysproxy::DEFAULT_BYPASS.to_owned()
}

fn default_ua() -> String {
    DEFAULT_SUBSCRIPTION_UA.to_owned()
}

fn default_true() -> bool {
    true
}

fn default_connection_log_limit() -> usize {
    1_000
}

fn default_operation_log_days() -> usize {
    30
}

pub fn load<T>(path: &Path, stage: &'static str) -> AppResult<T>
where
    T: DeserializeOwned + Default,
{
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(AppError::storage(stage, e)),
    };
    serde_json::from_slice(&bytes).map_err(|e| AppError::storage(stage, e))
}

pub fn save<T: Serialize>(path: &Path, value: &T, stage: &'static str) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| AppError::internal(stage, e))?;
    atomic::write(path, &bytes).map_err(|e| AppError::storage(stage, e))
}
