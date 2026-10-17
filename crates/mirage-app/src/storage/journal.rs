//! Recovery journal (DESIGN.md §11.3, §11.5): small files in `recovery/`
//! written before a system change, so a crash can be undone on next start.

use super::meta::{self, Versioned};
use crate::domain::error::AppResult;
use crate::platform::windows::process::ProcessIdentity;
use crate::platform::windows::sysproxy::ProxySettings;
use crate::storage::layout::Layout;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const STAGE: &str = "storage.journal";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SysProxyState {
    /// Journal written, WinINet not (yet) confirmed written.
    #[default]
    Pending,
    Applied,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SysProxyJournal {
    pub schema_version: u32,
    pub state: SysProxyState,
    pub original: ProxySettings,
    pub applied: ProxySettings,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CoreJournal {
    pub schema_version: u32,
    pub pid: u32,
    pub created: u64,
    pub image: String,
}

impl CoreJournal {
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            created: self.created,
            image: self.image.clone(),
        }
    }
}

impl Versioned for SysProxyJournal {
    fn set_schema_version(&mut self, v: u32) {
        self.schema_version = v;
    }
}

impl Versioned for CoreJournal {
    fn set_schema_version(&mut self, v: u32) {
        self.schema_version = v;
    }
}

pub fn sysproxy_path(layout: &Layout) -> PathBuf {
    layout.recovery.join("sysproxy.json")
}

pub fn core_path(layout: &Layout) -> PathBuf {
    layout.recovery.join("core.json")
}

/// `None` when no journal exists.
pub fn load_sysproxy(layout: &Layout) -> AppResult<Option<SysProxyJournal>> {
    load_existing(&sysproxy_path(layout))
}

pub fn save_sysproxy(layout: &Layout, journal: &mut SysProxyJournal) -> AppResult<()> {
    meta::save(&sysproxy_path(layout), journal, STAGE)
}

pub fn load_core(layout: &Layout) -> AppResult<Option<CoreJournal>> {
    load_existing(&core_path(layout))
}

pub fn save_core(layout: &Layout, journal: &mut CoreJournal) -> AppResult<()> {
    meta::save(&core_path(layout), journal, STAGE)
}

pub fn remove(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, file = %path.display(), "journal not removed"),
    }
}

fn load_existing<T>(path: &Path) -> AppResult<Option<T>>
where
    T: serde::de::DeserializeOwned + Default + Versioned,
{
    if !path.exists() {
        return Ok(None);
    }
    meta::load(path, STAGE).map(Some)
}
