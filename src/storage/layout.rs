//! Data directory layout (DESIGN.md §13).

use crate::platform::windows::{acl, known_folder};
use std::io;
use std::path::PathBuf;

const APP_DIR: &str = "Mirage";

pub struct Layout {
    pub root: PathBuf,
    pub logs: PathBuf,
    pub profiles: PathBuf,
    pub providers_cache: PathBuf,
    pub core_home: PathBuf,
    pub recovery: PathBuf,
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
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
