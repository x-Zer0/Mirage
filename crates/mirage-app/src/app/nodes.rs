//! Proxy groups, node selection and latency measurements.
use super::{AppCore, Internal, error_notice};
use crate::domain::error::AppError;
use crate::storage::meta;

const STAGE: &str = "app.handlers";

impl AppCore {
    pub(super) fn refresh_groups(&self) {
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        self.spawn(async move { Internal::Groups(generation, controller.groups().await) });
    }

    pub(super) fn on_groups(
        &mut self,
        generation: u64,
        result: Result<Vec<crate::cores::mihomo::controller::ProxyGroupView>, AppError>,
    ) {
        if generation != self.generation || self.running.is_none() {
            return;
        }
        match result {
            Ok(groups) => self.view.groups = groups,
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
    }

    pub(super) fn select_node(&mut self, group: String, node: String) {
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        let selection_path = self.layout.selection_file(&running.profile_id);
        self.spawn(async move {
            if let Err(e) = controller.select(&group, &node).await {
                return Internal::Groups(generation, Err(e));
            }
            if let Err(e) = remember_selection(&selection_path, group, node) {
                tracing::warn!(error = %e, "selection not saved");
            }
            Internal::Groups(generation, controller.groups().await)
        });
    }

    pub(super) fn test_delay(&mut self, group: String) {
        // §10.7: one group at a time, user initiated.
        if self.view.testing_group.is_some() {
            return;
        }
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        self.view.testing_group = Some(group.clone());
        self.spawn(async move {
            let result = controller.test_group_delay(&group).await;
            Internal::DelayDone(generation, group, result)
        });
    }
}

fn remember_selection(path: &std::path::Path, group: String, node: String) -> Result<(), AppError> {
    let mut file: meta::SelectionFile = meta::load(path, STAGE)?;
    file.groups.insert(group, node);
    meta::save(path, &mut file, STAGE)
}
