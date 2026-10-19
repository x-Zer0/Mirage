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
            Ok(groups) => {
                self.view.groups = groups;
                self.view.groups_revision += 1;
                self.view_dirty = true;
            }
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
    }

    pub(super) fn select_node(&mut self, group: String, node: String) {
        if self
            .view
            .groups
            .iter()
            .any(|g| g.name == group && g.now == node)
        {
            return;
        }
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        self.spawn(async move {
            let result = controller.select(&group, &node).await;
            Internal::NodeSelected {
                generation,
                group,
                node,
                result,
            }
        });
    }

    pub(super) fn on_node_selected(
        &mut self,
        generation: u64,
        group: String,
        node: String,
        result: Result<(), AppError>,
    ) {
        if generation != self.generation {
            return;
        }
        let (status, summary) = match &result {
            Ok(()) => {
                // Save selection in actor thread to avoid concurrent writes
                if let Some(running) = &self.running {
                    let selection_path = self.layout.selection_file(&running.profile_id);
                    if let Err(e) = remember_selection(&selection_path, group.clone(), node.clone())
                    {
                        tracing::warn!(error = %e, "selection not saved");
                    }
                }
                ("成功", format!("已选择节点：{node}"))
            }
            Err(e) => ("失败", e.user_message()),
        };
        self.record_operation("切换节点", &group, "手动", status, &summary);
        if let Err(e) = result {
            self.view.notice = Some(error_notice(&e));
        }
        self.refresh_groups();
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
