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
    /// Missing in files written before the field existed; the loader treats
    /// those as schema 0 and migrates them (§13).
    #[serde(default)]
    pub schema_version: u32,
    pub selected: Option<ProfileId>,
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RevisionsFile {
    #[serde(default)]
    pub schema_version: u32,
    pub revisions: Vec<ConfigRevision>,
}

/// `accounts.json` (DESIGN.md §13): provider accounts, SecretRefs only.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AccountsFile {
    #[serde(default)]
    pub schema_version: u32,
    pub accounts: Vec<ProviderAccount>,
}

/// `{group: node}` remembered per profile (DESIGN.md §10.6).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SelectionFile {
    #[serde(default)]
    pub schema_version: u32,
    pub groups: BTreeMap<String, String>,
}

/// UI dimensions in logical pixels; restored before the first window is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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

/// User preferences (DESIGN.md §13 `settings.json`). Fields added after M2
/// carry serde defaults, so older files load unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    #[serde(default)]
    pub ui_geometry: UiGeometry,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default = "default_true")]
    pub system_proxy: bool,
    #[serde(default)]
    pub tun_disable_system_proxy: bool,
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
    /// Start with Windows via the Run key with `--autostart` (§11.6).
    #[serde(default)]
    pub autostart: bool,
    /// Maximum connection rows kept for the current run (0 disables recording).
    #[serde(default = "default_connection_log_limit")]
    pub connection_log_limit: usize,
    /// Days of operation history to retain (0 disables recording).
    #[serde(default = "default_operation_log_days")]
    pub operation_log_days: usize,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            ui_geometry: UiGeometry::default(),
            schema_version: SCHEMA_VERSION,
            system_proxy: true,
            tun_disable_system_proxy: false,
            auto_connect: false,
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

/// Loads a file, migrating an older schema forward and refusing a newer one.
///
/// §13: before a file is migrated the previous content is copied to `<name>.bak`,
/// so a failed migration never destroys the only copy.
pub fn load<T>(path: &Path, stage: &'static str) -> AppResult<T>
where
    T: DeserializeOwned + Default + Versioned,
{
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(AppError::storage(stage, e)),
    };
    // The version is read from the raw document first: a file written before
    // the field existed has no version and is treated as 0.
    let found = read_version(&bytes).map_err(|e| AppError::storage(stage, e))?;
    if found > SCHEMA_VERSION {
        return Err(AppError::storage(
            stage,
            format!(
                "{} 由更新版本写入（schema {found}，本版本支持 {SCHEMA_VERSION}）",
                path.display()
            ),
        ));
    }
    let bytes = if found < SCHEMA_VERSION {
        let migrated =
            migrate(bytes, found, SCHEMA_VERSION).map_err(|e| AppError::storage(stage, e))?;
        backup(path).map_err(|e| AppError::storage(stage, e))?;
        atomic::write(path, &migrated).map_err(|e| AppError::storage(stage, e))?;
        tracing::info!(file = %path.display(), from = found, to = SCHEMA_VERSION, "metadata migrated");
        migrated
    } else {
        bytes
    };
    let value: T = serde_json::from_slice(&bytes).map_err(|e| AppError::storage(stage, e))?;
    Ok(value)
}

/// `schema_version` of a raw document; 0 when the field is absent or unreadable
/// as a version, which is what an M1-era file looks like.
fn read_version(bytes: &[u8]) -> Result<u32, String> {
    let doc: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("JSON 解析失败：{e}"))?;
    let version_u64 = doc
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    // M14: u32::try_from 失败按更新版本拒绝
    u32::try_from(version_u64)
        .map_err(|_| format!("schema_version 超出 u32 范围：{version_u64}，可能需要更新应用"))
}

/// Applies every migration step in order. Each step is a whole-document edit,
/// so a step can add, rename or drop fields as the schema evolves.
fn migrate(mut bytes: Vec<u8>, from: u32, to: u32) -> Result<Vec<u8>, String> {
    let mut doc: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("JSON 解析失败：{e}"))?;
    for version in from..to {
        step(&mut doc, version)
            .map_err(|e| format!("schema {version} → {} 迁移失败：{e}", version + 1))?;
    }
    if let Some(object) = doc.as_object_mut() {
        object.insert("schema_version".into(), serde_json::json!(to));
    }
    bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// One migration. Only step 0 → 1 exists so far: the file predates the
/// `schema_version` field and every later field is already covered by serde
/// defaults, so stamping the version is the whole migration.
fn step(doc: &mut serde_json::Value, from: u32) -> Result<(), String> {
    match from {
        0 => {
            if !doc.is_object() {
                return Err("顶层不是 JSON 对象".into());
            }
            Ok(())
        }
        other => Err(format!("没有从 schema {other} 开始的迁移")),
    }
}

/// Copies the current file to `<name>.bak` before it is migrated (§13).
fn backup(path: &Path) -> std::io::Result<()> {
    let backup = path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json")
    ));
    std::fs::copy(path, &backup).map(|_| ())
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

/// Metadata documents carry a schema version. `load` reads it from the raw
/// document (so a file written before the field existed reads as 0), and
/// `save` always stamps the current value.
pub trait Versioned {
    fn set_schema_version(&mut self, v: u32);
}

macro_rules! versioned {
    ($($t:ty),*) => {$(
        impl Versioned for $t {
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
