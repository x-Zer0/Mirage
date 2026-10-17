//! Command and completion handlers for [`AppCore`].

use super::state::{Connectivity, ProfileView, Retry, RuntimeStatus};
use super::{AppCore, Command, Internal, error_notice, info_notice, pipeline, runtime, updates};
use crate::domain::error::{AppError, AppErrorKind};
use crate::domain::model::{EgressPolicy, Profile, SubscriptionSource, Validity, now};
use crate::platform::windows::conflict::{self, Conflict};
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
            Command::AddRemoteProfile { name, url } => self.add_remote(name, url),
            Command::UpdateProfile(id) => self.update_profile(&id, true),
            Command::UpdateAll => self.update_all(),
            Command::ProviderLogin { email, password } => self.provider_login(email, password),
            Command::RevalidateAccount(id) => self.revalidate_account(&id, true),
            Command::RemoveAccount(id, with_profile) => self.remove_account(&id, with_profile),
            Command::ImportLegacyZiyoumao if !self.core_checked() => {
                self.deferred.push(Command::ImportLegacyZiyoumao);
            }
            Command::ImportLegacyZiyoumao => self.import_legacy(),
            Command::ProbeProviderHosts => self.probe_provider_hosts(),
            Command::Connect => self.connect(),
            Command::Disconnect => self.disconnect(),
            Command::SelectNode { group, node } => self.select_node(group, node),
            Command::TestGroupDelay(group) => self.test_delay(group),
            Command::RefreshGroups => self.refresh_groups(),
            Command::SetSystemProxy(on) => self.set_system_proxy(on),
            Command::SetTun(on) => self.set_tun(on),
            Command::ManageService(install) => self.manage_service(install, false),
            Command::CheckConnectivity => self.check_connectivity(),
            Command::DismissNotice => self.view.error = None,
            Command::RetryNotice => self.retry(),
            Command::SetRoutingMode(mode) => self.set_routing_mode(mode),
            Command::SaveSettings(input) => self.save_settings(input),
            Command::SetUpdatePolicy {
                id,
                interval_hours,
                egress,
            } => self.set_update_policy(&id, interval_hours, egress),
            Command::CopyProfileUrl(id) => self.copy_profile_url(&id),
            Command::UpdateGeodata => self.update_geodata(),
            Command::WindowVisible(visible) => self.set_window_visible(visible),
            Command::CloseHintShown => self.close_hint_shown(),
            Command::ExportDiagnostics => self.export_diagnostics(),
            Command::SetLiveSink(sink) => self.live.set_sink(sink),
            // Both end the actor loop before reaching here (app::AppCore::run).
            Command::SessionEnding(_) | Command::Quit => {}
        }
    }

    pub(super) fn handle_internal(&mut self, msg: Internal) {
        match msg {
            Internal::ServiceManaged {
                generation,
                install,
                enable,
                result,
            } => self.on_service_managed(generation, install, enable, result),
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
            Internal::RestartDue(generation) => self.on_restart_due(generation),
            Internal::Connectivity(generation, result) => self.on_connectivity(generation, result),
            Internal::RemoteAdded(added) => self.on_remote_added(*added),
            Internal::Updated(outcome, manual) => self.on_updated(*outcome, manual),
            Internal::Reloaded {
                generation,
                profile_id,
                revision,
                outcome,
            } => self.on_reloaded(generation, profile_id, revision, outcome),
            Internal::LoggedIn(result) => self.on_logged_in(*result),
            Internal::ProviderProfileAdded { account_id, result } => {
                self.on_provider_profile_added(account_id, *result)
            }
            Internal::AccountRefreshed {
                account_id,
                result,
                manual,
            } => self.on_account_refreshed(account_id, result, manual),
            Internal::HostsProbed(result) => self.on_hosts_probed(result),
            Internal::ModeApplied(generation, result) => self.on_mode_applied(generation, result),
            Internal::CoreMode(generation, result) => self.on_core_mode(generation, result),
            Internal::GeoUpdated(result) => self.on_geo_updated(result),
            Internal::DiagnosticsExported(result) => self.on_diagnostics_exported(result),
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
        // §16.1: start from the stored revision, never waiting on the network.
        if self.binary.is_some() && self.settings.auto_connect && self.profiles.selected().is_some()
        {
            tracing::info!("auto-connect on startup");
            self.connect();
        }
    }

    pub(super) fn require_binary(&mut self) -> Option<crate::cores::mihomo::binary::CoreBinary> {
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
            // Local files are never fetched; the interval only matters for URLs.
            update_policy: Default::default(),
            usage: None,
            last_fetch: None,
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

    pub(super) fn remove_profile(&mut self, id: &str) {
        if self.running.as_ref().is_some_and(|r| r.profile_id == id) {
            self.disconnect();
        }
        if let Some(removed) = self.profiles.remove(id) {
            self.save_profiles();
            self.forget_secrets(&removed);
            if self
                .queued_reload
                .as_ref()
                .is_some_and(|(queued, _)| queued == id)
            {
                self.queued_reload = None;
            }
            let dir = self.layout.profile_dir(&id.to_owned());
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(error = %e, "profile directory not removed");
            }
        }
        self.refresh_profiles_view();
    }

    fn connect(&mut self) {
        if self.view.service_busy {
            return;
        }
        if matches!(
            self.view.runtime,
            RuntimeStatus::Starting
                | RuntimeStatus::Running
                | RuntimeStatus::Reloading
                | RuntimeStatus::Stopping
        ) {
            return;
        }
        // §11.8: warn about the original client before starting. This never
        // blocks the connect: the user may be running it deliberately, and the
        // check exists to explain a failure, not to prevent an attempt.
        let conflicts = conflict::detect(self.view.mixed_port);
        if !conflicts.is_empty() {
            tracing::warn!(
                count = conflicts.len(),
                "conflicts with the original client"
            );
            let text = conflicts
                .iter()
                .map(Conflict::text)
                .collect::<Vec<_>>()
                .join("；");
            self.view.notice = Some(
                info_notice(format!("提示：{text}。若连接失败请先完全退出原客户端。")).notifying(),
            );
        }
        // An explicit connect starts a fresh restart budget and re-reads the
        // saved switch (an external change may have turned it off for this run).
        self.restart_attempts = 0;
        self.view.system_proxy_wanted = self.settings.system_proxy;
        self.start_after(None);
    }

    /// Starts the selected profile, first stopping `previous` in the same task.
    pub(super) fn start_after(&mut self, previous: Option<runtime::Running>) {
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
        self.view.error = None;
        let req = runtime::StartRequest {
            generation,
            layout: self.layout.clone(),
            binary,
            profile,
            mixed_port: self.view.mixed_port,
            mode: self.settings.routing_mode,
            tun: self.view.tun_wanted,
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
                if self.view.runtime == RuntimeStatus::Restarting {
                    tracing::info!(attempt = self.restart_attempts, "core restarted");
                    self.view.notice = Some(info_notice("内核已自动重启"));
                }
                self.view.runtime = RuntimeStatus::Running;
                self.view.check_lines = running.check_lines.clone();
                self.view.policy_notes = running.policy_notes.clone();
                self.update_tun_state(running.process.service().is_some());
                self.watch_exit(&running);
                self.running = Some(running);
                self.ensure_sysproxy();
                self.refresh_groups();
                self.read_core_mode();
                self.check_connectivity();
                // The controller address changed with the new process.
                self.restart_live_streams();
            }
            Ok(stale) => {
                // A newer transaction superseded this start; discard its core.
                let layout = self.layout.clone();
                tokio::spawn(async move { runtime::stop(stale, &layout).await });
            }
            Err(e) if generation == self.generation => {
                if self.view.tun_wanted && self.view.runtime != RuntimeStatus::Restarting {
                    self.view.tun_wanted = false;
                    self.update_tun_state(false);
                    self.start_after(None);
                    self.view.notice = Some(error_notice(&e.append("；正在恢复普通代理模式")));
                    return;
                }
                if self.view.runtime == RuntimeStatus::Restarting {
                    self.schedule_restart(e.user_message());
                } else {
                    // A failed user connect may follow a crash; never leave the
                    // system proxy pointing at a port nobody listens on.
                    self.release_sysproxy();
                    // The status names the failure; the error bar carries the
                    // detail (which may include the core's log tail).
                    let reason = if e.kind == AppErrorKind::ServiceUnavailable {
                        e.user_message()
                    } else {
                        e.kind.label().to_owned()
                    };
                    self.view.runtime = RuntimeStatus::Failed(reason);
                    self.view.notice = Some(error_notice(&e).with_retry(Retry::Connect));
                }
            }
            Err(_) => {}
        }
    }

    /// User disconnect: system proxy first, then the core (§11.4).
    fn disconnect(&mut self) {
        self.generation += 1;
        self.restart_attempts = 0;
        self.release_sysproxy();
        self.update_tun_state(false);
        self.view.groups.clear();
        self.view.testing_group = None;
        self.view.connectivity = Connectivity::Unknown;
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

    pub(super) fn refresh_groups(&self) {
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
            .map(|p| profile_view(&self.layout, p, &selected, self.updating.contains(&p.id)))
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
    updating: bool,
) -> ProfileView {
    let active = p
        .active_revision
        .as_ref()
        .and_then(|rev| revisions::find(layout, p, rev).ok().flatten());
    let detail = active
        .as_ref()
        .map(|r| match &r.validity {
            Validity::Valid => format!(
                "{} 个节点 · {} 个组 · {} 条规则",
                r.summary.proxies, r.summary.groups, r.summary.rules
            ),
            Validity::Invalid(reason) => format!("无效：{reason}"),
            Validity::Unchecked => "未校验".into(),
        })
        .unwrap_or_else(|| "没有可用版本".into());
    let source = match &p.source {
        SubscriptionSource::RemoteUrl { host, .. } => format!("订阅 {host}"),
        other => other.label().into(),
    };
    ProfileView {
        id: p.id.clone(),
        name: p.name.clone(),
        source,
        detail,
        updated: updates::profile_time(p.updated_at),
        selected: p.id == selected,
        remote: p.is_remote(),
        updating,
        usage: p.usage.map(usage_text).unwrap_or_default(),
        fetch_error: p
            .last_fetch
            .as_ref()
            .and_then(|f| f.error.clone())
            .unwrap_or_default(),
        has_url: matches!(p.source, SubscriptionSource::RemoteUrl { .. }),
        interval_hours: p.update_policy.interval_hours,
        egress: EgressPolicy::ALL
            .iter()
            .position(|e| *e == p.update_policy.egress)
            .unwrap_or(0),
        policy_notes: active.map(|r| r.policy_notes).unwrap_or_default(),
    }
}

/// "已用 1.2 GiB / 100.0 GiB · 到期 2026-12-01"; expiry 0/missing = 未提供 (§15).
fn usage_text(u: crate::domain::model::UsageInfo) -> String {
    let used = gib(u.upload.saturating_add(u.download));
    let total = if u.total > 0 {
        gib(u.total)
    } else {
        "不限".into()
    };
    let expire = u
        .expire
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "未提供".into());
    format!("已用 {used} / {total} · 到期 {expire}")
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}
