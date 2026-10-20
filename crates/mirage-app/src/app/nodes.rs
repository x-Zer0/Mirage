//! Proxy groups, node selection and latency measurements.
use super::{AppCore, Internal, error_notice};
use crate::cores::mihomo::node_metadata;
use crate::domain::error::AppError;
use crate::storage::meta;
use std::collections::HashMap;

const STAGE: &str = "app.handlers";

impl AppCore {
    pub(super) fn refresh_groups(&self) {
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        let epoch = self.delay_tests.groups_epoch;
        let layout = self.layout.clone();
        let service = running.process.service().is_some();
        self.spawn(async move {
            let mut result = controller.groups().await;
            if let Ok(groups) = &mut result {
                node_metadata::apply(groups, &layout, service).await;
            }
            Internal::Groups(generation, epoch, result)
        });
    }

    pub(super) fn on_groups(
        &mut self,
        generation: u64,
        epoch: u64,
        result: Result<Vec<crate::cores::mihomo::controller::ProxyGroupView>, AppError>,
    ) {
        if generation != self.generation
            || epoch != self.delay_tests.groups_epoch
            || self.running.is_none()
        {
            return;
        }
        match result {
            Ok(mut groups) => {
                let previous: HashMap<_, _> = self
                    .view
                    .groups
                    .iter()
                    .flat_map(|g| &g.nodes)
                    .map(|n| (n.name.as_str(), n))
                    .collect();
                for node in groups.iter_mut().flat_map(|g| &mut g.nodes) {
                    if let Some(old) = previous.get(node.name.as_str()) {
                        node.delay = old.delay.clone().filter(|d| d.is_fresh());
                        node.test_state = old.test_state;
                    }
                }
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
}

fn remember_selection(path: &std::path::Path, group: String, node: String) -> Result<(), AppError> {
    let mut file: meta::SelectionFile = meta::load(path, STAGE)?;
    file.groups.insert(group, node);
    meta::save(path, &file, STAGE)
}
