//! Data directory layout (DESIGN.md §10.2, §13).

use crate::domain::model::{ProfileId, RevisionId};
use crate::platform::windows::{acl, known_folder};
use std::io;
use std::path::PathBuf;

const APP_DIR: &str = "Mirage";

#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
    pub logs: PathBuf,
    pub profiles: PathBuf,
    pub providers_cache: PathBuf,
    pub core_home: PathBuf,
    pub recovery: PathBuf,
    /// Directory holding the bundled `mihomo.exe` and geodata.
    pub bundled_core: PathBuf,
}

impl Layout {
    pub fn resolve() -> io::Result<Self> {
        let root = known_folder::local_app_data()?.join(APP_DIR);
        Ok(Self {
            logs: root.join("logs"),
            profiles: root.join("profiles"),
            providers_cache: root.join("providers-cache"),
            core_home: root.join("core-home"),
            recovery: root.join("recovery"),
            bundled_core: bundled_core_dir()?,
            root,
        })
    }

    /// Creates the tree and restricts the root to the current user + SYSTEM.
    /// The ACL is applied on every start so a loosened directory is repaired.
    pub fn prepare(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        acl::restrict_to_owner(&self.root)?;
        for dir in [
            &self.logs,
            &self.profiles,
            &self.providers_cache,
            &self.core_home,
            &self.recovery,
            &self.core_check_dir(),
            &self.core_run_dir(),
            &self.core_home.join("logs"),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// DPAPI files for secrets above the Credential Manager blob limit (§14.1).
    pub fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }

    pub fn profiles_file(&self) -> PathBuf {
        self.root.join("profiles.json")
    }

    pub fn profile_dir(&self, id: &ProfileId) -> PathBuf {
        self.profiles.join(id)
    }

    pub fn raw_revision(&self, id: &ProfileId, rev: &RevisionId) -> PathBuf {
        self.profile_dir(id).join("raw").join(format!("{rev}.bin"))
    }

    pub fn revisions_file(&self, id: &ProfileId) -> PathBuf {
        self.profile_dir(id).join("revisions.json")
    }

    pub fn selection_file(&self, id: &ProfileId) -> PathBuf {
        self.profile_dir(id).join("selection.json")
    }

    pub fn core_check_dir(&self) -> PathBuf {
        self.core_home.join("check")
    }

    pub fn core_run_dir(&self) -> PathBuf {
        self.core_home.join("run")
    }

    pub fn run_config(&self) -> PathBuf {
        self.core_run_dir().join("config.yaml")
    }
}

/// `<exe dir>\core` when installed; falls back to `<crate>\assets\core` in dev builds.
fn bundled_core_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let installed = exe.parent().map(|d| d.join("core")).unwrap_or_default();
    if installed.join("mihomo.exe").is_file() {
        return Ok(installed);
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("core");
    Ok(if dev.join("mihomo.exe").is_file() {
        dev
    } else {
        installed
    })
}
