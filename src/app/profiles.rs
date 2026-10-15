//! Profile list persistence (DESIGN.md §13 `profiles.json`).

use crate::domain::error::{AppError, AppResult};
use crate::domain::model::{Profile, ProfileId};
use crate::storage::layout::Layout;
use crate::storage::meta::{self, ProfilesFile};

const STAGE: &str = "app.profiles";

pub struct Profiles {
    file: ProfilesFile,
}

impl Profiles {
    pub fn empty() -> Self {
        Self {
            file: ProfilesFile::default(),
        }
    }

    pub fn load(layout: &Layout) -> AppResult<Self> {
        Ok(Self {
            file: meta::load(&layout.profiles_file(), STAGE)?,
        })
    }

    pub fn save(&mut self, layout: &Layout) -> AppResult<()> {
        meta::save(&layout.profiles_file(), &mut self.file, STAGE)
    }

    pub fn all(&self) -> &[Profile] {
        &self.file.profiles
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.file.profiles.iter().find(|p| p.id == id)
    }

    pub fn selected_id(&self) -> Option<&ProfileId> {
        self.file.selected.as_ref()
    }

    pub fn selected(&self) -> Option<&Profile> {
        self.selected_id().and_then(|id| self.get(id))
    }

    pub fn select(&mut self, id: &str) -> AppResult<()> {
        if self.get(id).is_none() {
            return Err(AppError::internal(STAGE, "配置不存在"));
        }
        self.file.selected = Some(id.to_owned());
        Ok(())
    }

    pub fn add(&mut self, profile: Profile) {
        if self.file.selected.is_none() {
            self.file.selected = Some(profile.id.clone());
        }
        self.file.profiles.push(profile);
    }

    pub fn remove(&mut self, id: &str) -> Option<Profile> {
        let index = self.file.profiles.iter().position(|p| p.id == id)?;
        let removed = self.file.profiles.remove(index);
        if self.file.selected.as_deref() == Some(id) {
            self.file.selected = self.file.profiles.first().map(|p| p.id.clone());
        }
        Some(removed)
    }
}
