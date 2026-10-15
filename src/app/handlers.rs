//! Command and completion handlers for [`AppCore`].

use super::state::{ProfileView, RuntimeStatus};
use super::{AppCore, Command, Internal, error_notice, info_notice, pipeline, runtime};
use crate::domain::error::{AppError, AppErrorKind};
use crate::domain::model::{Profile, SubscriptionSource, Validity, now};
use crate::storage::meta;
use crate::subscriptions::revisions;

const STAGE: &str = "app.handlers";

impl AppCore {
    pub(super) fn handle(&mut self, cmd: Command) {
        tracing::debug!(?cmd, "command");
        match cmd {
            Command::PickAndImport => self.pick_and_import(),
            Command::ImportLocalFile(path) if !self.core_checked() => {
                self.deferred.push(Command::ImportLocalFile(path));
            }
            Command::ImportLocalFile(path) => self.import(path),
            Command::SelectProfile(id) => self.select_profile(&id),
            Command::RemoveProfile(id) => self.remove_profile(&id),
            Command::Connect => self.connect(),
            Command::Disconnect => self.disconnect(),
            Command::SelectNode { group, node } => self.select_node(group, node),
            Command::TestGroupDelay(group) => self.test_delay(group),
            Command::RefreshGroups => self.refresh_groups(),
            Command::DismissNotice => self.view.notice = None,
            Command::Quit => {}
        }
    }

    pub(super) fn handle_internal(&mut self, msg: Internal) {
        match msg {
            Internal::CoreVerified(result) => self.on_core_verified(result),
            Internal::Imported(result) => self.on_imported(result),
            Internal::Started(generation, result) => self.on_started(generation, result),
            Internal::CoreExited { generation, detail } => self.on_core_exited(generation, detail),
            Internal::Groups(generation, result) => self.on_groups(generation, result),
            Internal::Stopped(generation) => {
                if generation == self.generation && self.view.runtime == RuntimeStatus::Stopping {
                    self.view.runtime = RuntimeStatus::Stopped;
                }
            }
            Internal::DelayDone(generation, group, result) => {
                if self.view.testing_group.as_deref() == Some(group.as_str()) {
                    self.view.testing_group = None;
                }
                match result {
                    Ok(()) => self.refresh_groups(),
                    Err(e) if generation == self.generation => {
                        self.view.notice = Some(error_notice(&e));
                    }
                    Err(_) => {}
                }
            }
        }
    }

    fn core_checked(&self) -> bool {
        self.binary.is_some() || self.view.core_version == "不可用"
    }

    fn on_core_verified(
        &mut self,
        result: Result<crate::cores::mihomo::binary::CoreBinary, AppError>,
    ) {
        match result {
            Ok(binary) => {
                tracing::info!(version = %binary.version_line, "core verified");
                self.view.core_version = binary.short_version();
                self.view.core_ready = true;
                self.binary = Some(binary);
            }
            Err(e) => {
                self.view.core_version = "不可用".into();
                self.view.notice = Some(error_notice(&e));
            }
        }
        for cmd in std::mem::take(&mut self.deferred) {
            self.handle(cmd);
        }
    }

    fn require_binary(&mut self) -> Option<crate::cores::mihomo::binary::CoreBinary> {
        if self.binary.is_none() {
            self.view.notice = Some(error_notice(&AppError::new(
                AppErrorKind::CoreBinaryInvalid,
                STAGE,
                "内核尚未通过校验",
            )));
        }
        self.binary.clone()
    }

    fn pick_and_import(&mut self) {
        let tx = self.internal_tx.clone();
        let layout = self.layout.clone();
        let Some(binary) = self.require_binary() else {
            return;
        };
        let port = self.view.mixed_port;
        self.view.busy = Some("正在导入配置…".into());
        tokio::spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("导入 Mihomo 配置")
                .add_filter("YAML 配置", &["yaml", "yml"])
                .add_filter("所有文件", &["*"])
                .pick_file()
                .await;
            let result = match picked {
                Some(file) => pipeline::import_local(&binary, &layout, file.path(), port).await,
                None => Err(AppError::new(AppErrorKind::Cancelled, STAGE, "")),
            };
            let _ = tx.send(Internal::Imported(result));
        });
    }

    fn import(&mut self, path: std::path::PathBuf) {
        let Some(binary) = self.require_binary() else {
            return;
        };
        let layout = self.layout.clone();
        let port = self.view.mixed_port;
        self.view.busy = Some("正在导入配置…".into());
        self.spawn(async move {
            Internal::Imported(pipeline::import_local(&binary, &layout, &path, port).await)
        });
    }

    fn on_imported(&mut self, result: Result<pipeline::ImportOutcome, AppError>) {
        self.view.busy = None;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) if e.kind == AppErrorKind::Cancelled => return,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        let profile = Profile {
            id: outcome.profile_id.clone(),
            name: outcome.name.clone(),
            source: SubscriptionSource::LocalFile {
                original_name: outcome.name.clone(),
                imported_at: now(),
            },
            active_revision: Some(outcome.revision.id.clone()),
            last_good_revision: Some(outcome.revision.id.clone()),
            updated_at: now(),
        };
        let stored =
            revisions::store_raw(&self.layout, &profile, &outcome.revision.id, &outcome.raw)
                .and_then(|()| revisions::upsert(&self.layout, &profile, outcome.revision.clone()));
        if let Err(e) = stored {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.profiles.add(profile);
        if let Err(e) = self.profiles.save(&self.layout) {
            self.view.notice = Some(error_notice(&e));
        }
        self.view.policy_notes = outcome.revision.policy_notes.clone();
        self.view.check_lines = outcome.check_lines;
        let s = outcome.revision.summary;
        self.view.notice = Some(info_notice(format!(
            "已导入“{}”：{} 个节点，{} 个组，{} 条规则",
            outcome.name, s.proxies, s.groups, s.rules
        )));
        self.refresh_profiles_view();
    }

    fn select_profile(&mut self, id: &str) {
        if self.profiles.selected_id().map(String::as_str) == Some(id) {
            return;
        }
        if let Err(e) = self
            .profiles
            .select(id)
            .and_then(|()| self.profiles.save(&self.layout))
        {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.refresh_profiles_view();
        if let Some(previous) = self.running.take() {
            // Switching is one commit transaction: the old core must release the
            // port before the new one starts, so both steps run in one task.
            self.view.groups.clear();
            self.start_after(Some(previous));
        }
    }

    fn remove_profile(&mut self, id: &str) {
        if self.running.as_ref().is_some_and(|r| r.profile_id == id) {
            self.disconnect();
        }
        if self.profiles.remove(id).is_some() {
            if let Err(e) = self.profiles.save(&self.layout) {
                self.view.notice = Some(error_notice(&e));
            }
            let dir = self.layout.profile_dir(&id.to_owned());
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(error = %e, "profile directory not removed");
            }
        }
        self.refresh_profiles_view();
    }

    fn connect(&mut self) {
        if matches!(
            self.view.runtime,
            RuntimeStatus::Starting | RuntimeStatus::Running | RuntimeStatus::Stopping
        ) {
            return;
        }
        self.start_after(None);
    }

    /// Starts the selected profile, first stopping `previous` in the same task.
    fn start_after(&mut self, previous: Option<runtime::Running>) {
        let Some(binary) = self.require_binary() else {
            return;
        };
        let Some(profile) = self.profiles.selected().cloned() else {
            self.view.notice = Some(info_notice("请先导入并选择一个配置"));
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.view.runtime = RuntimeStatus::Starting;
        self.view.notice = None;
        let req = runtime::StartRequest {
            generation,
            layout: self.layout.clone(),
            binary,
            profile,
            mixed_port: self.view.mixed_port,
        };
        self.spawn(async move {
            if let Some(previous) = previous {
                runtime::stop(previous, &req.layout).await;
            }
            Internal::Started(generation, runtime::start(req).await)
        });
    }

    fn on_started(&mut self, generation: u64, result: Result<runtime::Running, AppError>) {
        match result {
            Ok(running) if generation == self.generation => {
                self.view.runtime = RuntimeStatus::Running;
                self.view.check_lines = running.check_lines.clone();
                self.watch_exit(&running);
                self.running = Some(running);
                self.refresh_groups();
            }
            Ok(stale) => {
                // A newer transaction superseded this start; discard its core.
                let layout = self.layout.clone();
                tokio::spawn(async move { runtime::stop(stale, &layout).await });
            }
            Err(e) if generation == self.generation => {
                self.view.runtime = RuntimeStatus::Failed(e.user_message());
                self.view.notice = Some(error_notice(&e));
            }
            Err(_) => {}
        }
    }

    fn watch_exit(&self, running: &runtime::Running) {
        let mut exit = running.process.exit_watch();
        let generation = running.generation;
        let log = running.log_file.clone();
        self.spawn(async move {
            let _ = exit.wait_for(Option::is_some).await;
            let code = exit.borrow().and_then(|e| e.code);
            let tail = crate::cores::mihomo::process::log_tail(&log, 3);
            Internal::CoreExited {
                generation,
                detail: format!("退出码 {code:?}；{tail}"),
            }
        });
    }

    fn on_core_exited(&mut self, generation: u64, detail: String) {
        if generation != self.generation || self.running.is_none() {
            return; // expected exit after a stop or a superseded start
        }
        // M1 reports the crash; supervised restart and sysproxy rollback land in M2 (§5.2, §11.4).
        self.running = None;
        runtime::remove_run_config(&self.layout);
        let e = AppError::new(AppErrorKind::CoreExited, STAGE, detail);
        self.view.runtime = RuntimeStatus::Failed(e.user_message());
        self.view.groups.clear();
        self.view.notice = Some(error_notice(&e));
    }

    fn disconnect(&mut self) {
        self.generation += 1;
        self.view.groups.clear();
        self.view.testing_group = None;
        let Some(running) = self.running.take() else {
            self.view.runtime = RuntimeStatus::Stopped;
            return;
        };
        self.view.runtime = RuntimeStatus::Stopping;
        let layout = self.layout.clone();
        let generation = self.generation;
        self.spawn(async move {
            runtime::stop(running, &layout).await;
            Internal::Stopped(generation)
        });
    }

    fn refresh_groups(&self) {
        let Some(running) = &self.running else { return };
        let controller = running.controller.clone();
        let generation = self.generation;
        self.spawn(async move { Internal::Groups(generation, controller.groups().await) });
    }

    fn on_groups(
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

    fn select_node(&mut self, group: String, node: String) {
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

    fn test_delay(&mut self, group: String) {
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

    pub(super) fn refresh_profiles_view(&mut self) {
        let selected = self.profiles.selected_id().cloned().unwrap_or_default();
        self.view.selected_profile = selected.clone();
        self.view.profiles = self
            .profiles
            .all()
            .iter()
            .map(|p| profile_view(&self.layout, p, &selected))
            .collect();
    }
}

fn remember_selection(path: &std::path::Path, group: String, node: String) -> Result<(), AppError> {
    let mut file: meta::SelectionFile = meta::load(path, STAGE)?;
    file.groups.insert(group, node);
    meta::save(path, &mut file, STAGE)
}

fn profile_view(
    layout: &crate::storage::layout::Layout,
    p: &Profile,
    selected: &str,
) -> ProfileView {
    let detail = p
        .active_revision
        .as_ref()
        .and_then(|rev| revisions::find(layout, p, rev).ok().flatten())
        .map(|r| match &r.validity {
            Validity::Valid => format!(
                "{} 个节点 · {} 个组 · {} 条规则",
                r.summary.proxies, r.summary.groups, r.summary.rules
            ),
            Validity::Invalid(reason) => format!("无效：{reason}"),
            Validity::Unchecked => "未校验".into(),
        })
        .unwrap_or_else(|| "没有可用版本".into());
    ProfileView {
        id: p.id.clone(),
        name: p.name.clone(),
        source: p.source.label().into(),
        detail,
        updated: chrono::DateTime::from_timestamp(p.updated_at, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default(),
        selected: p.id == selected,
    }
}
