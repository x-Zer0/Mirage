//! JSON metadata files with a schema version (DESIGN.md §13).

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

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProfilesFile {
    pub schema_version: u32,
    pub selected: Option<ProfileId>,
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RevisionsFile {
    pub schema_version: u32,
    pub revisions: Vec<ConfigRevision>,
}

/// `accounts.json` (DESIGN.md §13): provider accounts, SecretRefs only.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AccountsFile {
    pub schema_version: u32,
    pub accounts: Vec<ProviderAccount>,
}

/// `{group: node}` remembered per profile (DESIGN.md §10.6).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SelectionFile {
    pub schema_version: u32,
    pub groups: BTreeMap<String, String>,
}

/// User preferences (DESIGN.md §13 `settings.json`). Fields added after M2
/// carry serde defaults, so older files load unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    pub schema_version: u32,
    #[serde(default = "default_true")]
    pub system_proxy: bool,
    /// Connect with the active revision once the core is verified (§16.1).
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default = "default_mixed_port")]
    pub mixed_port: u16,
    /// WinINet bypass list written with the system proxy (§11.2).
    #[serde(default = "default_bypass")]
    pub bypass: String,
    /// UA for plain URL subscriptions without their own (§15).
    #[serde(default = "default_ua")]
    pub default_user_agent: String,
    /// Egress of the provider account API (§12.2).
    #[serde(default)]
    pub provider_egress: EgressPolicy,
    /// `None`: keep the mode the config declares (§9.2).
    #[serde(default)]
    pub routing_mode: Option<RoutingMode>,
    /// Closing the window hides it to the tray instead of quitting (§17.2).
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    /// The one-time "still running in the tray" hint was shown.
    #[serde(default)]
    pub close_hint_shown: bool,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            system_proxy: true,
            auto_connect: false,
            mixed_port: default_mixed_port(),
            bypass: default_bypass(),
            default_user_agent: default_ua(),
            provider_egress: EgressPolicy::default(),
            routing_mode: None,
            close_to_tray: true,
            close_hint_shown: false,
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

/// Loads a file, returning `T::default()` when it does not exist yet.
/// A file written by a newer schema is refused rather than silently downgraded.
pub fn load<T>(path: &Path, stage: &'static str) -> AppResult<T>
where
    T: DeserializeOwned + Default + Versioned,
{
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(AppError::storage(stage, e)),
    };
    let value: T = serde_json::from_slice(&bytes).map_err(|e| AppError::storage(stage, e))?;
    if value.schema_version() > SCHEMA_VERSION {
        return Err(AppError::storage(
            stage,
            format!(
                "{} 由更新版本写入（schema {}）",
                path.display(),
                value.schema_version()
            ),
        ));
    }
    Ok(value)
}

pub fn save<T: Serialize + Versioned>(
    path: &Path,
    value: &mut T,
    stage: &'static str,
) -> AppResult<()> {
    value.set_schema_version(SCHEMA_VERSION);
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| AppError::internal(stage, e))?;
    atomic::write(path, &bytes).map_err(|e| AppError::storage(stage, e))
}

pub trait Versioned {
    fn schema_version(&self) -> u32;
    fn set_schema_version(&mut self, v: u32);
}

macro_rules! versioned {
    ($($t:ty),*) => {$(
        impl Versioned for $t {
            fn schema_version(&self) -> u32 { self.schema_version }
            fn set_schema_version(&mut self, v: u32) { self.schema_version = v; }
        }
    )*};
}
versioned!(
    ProfilesFile,
    RevisionsFile,
    SelectionFile,
    SettingsFile,
    AccountsFile
);
