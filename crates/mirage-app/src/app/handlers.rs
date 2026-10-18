//! Dispatches UI commands and background completions to feature modules.
use super::state::RuntimeStatus;
use super::{AppCore, Command, Internal, error_notice};

impl AppCore {
    pub(super) fn handle(&mut self, cmd: Command) {
        if !matches!(&cmd, Command::QueryLogs(_)) {
            tracing::debug!(?cmd, "command");
        }
        match cmd {
            Command::QueryLogs(query) => self.query_logs(query),
            Command::ClearConnectionLogs => self.clear_connection_logs(),
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
            Command::ProviderLogin {
                provider_id,
                email,
                password,
            } => self.provider_login(provider_id, email, password),
            Command::RevalidateAccount(id) => self.revalidate_account(&id, true),
            Command::RevealAccount { id, visible } => self.reveal_account(&id, visible),
            Command::ImportLegacyProvider(provider_id) if !self.core_checked() => {
                self.deferred
                    .push(Command::ImportLegacyProvider(provider_id));
            }
            Command::ImportLegacyProvider(provider_id) => self.import_legacy(&provider_id),
            Command::ProbeProviderHosts => self.probe_provider_hosts(),
            Command::Connect => self.connect(true),
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
            Internal::Connections(epoch, rows) => self.on_connections(epoch, rows),
            Internal::LogStreamStatus(generation, status) => {
                if generation == self.live.epoch {
                    self.set_log_status(status);
                }
            }
            Internal::NodeSelected {
                generation,
                group,
                node,
                result,
            } => self.on_node_selected(generation, group, node, result),
            Internal::ServiceManaged {
                generation,
                install,
                enable,
                result,
            } => self.on_service_managed(generation, install, enable, result),
            Internal::CoreVerified(result) => self.on_core_verified(result),
            Internal::Imported(object, result) => self.on_imported(object, result),
            Internal::Started(generation, result) => self.on_started(generation, result),
            Internal::CoreExited { generation, detail } => self.on_core_exited(generation, detail),
            Internal::Groups(generation, result) => self.on_groups(generation, result),
            Internal::Stopped(generation) => {
                if generation == self.generation && self.view.runtime == RuntimeStatus::Stopping {
                    self.view.runtime = RuntimeStatus::Stopped;
                    self.record_operation("断开连接", "", "系统", "成功", "代理内核已停止");
                }
            }
            Internal::DelayDone(generation, group, result) => {
                if self.view.testing_group.as_deref() == Some(group.as_str()) {
                    self.view.testing_group = None;
                }
                match result {
                    Ok(()) if generation == self.generation => self.refresh_groups(),
                    Err(e) if generation == self.generation => {
                        self.refresh_groups();
                        self.view.notice = Some(error_notice(&e));
                    }
                    _ => {}
                }
            }
            Internal::RestartDue(generation) => self.on_restart_due(generation),
            Internal::Connectivity(generation, request, result) => {
                self.on_connectivity(generation, request, result)
            }
            Internal::RemoteAdded(added) => self.on_remote_added(*added),
            Internal::Updated(outcome, manual) => self.on_updated(*outcome, manual),
            Internal::Reloaded {
                generation,
                profile_id,
                revision,
                outcome,
            } => self.on_reloaded(generation, profile_id, revision, outcome),
            Internal::LoggedIn {
                provider_id,
                result,
            } => self.on_logged_in(provider_id, *result),
            Internal::ProviderProfileAdded { account_id, result } => {
                self.on_provider_profile_added(account_id, *result)
            }
            Internal::AccountRefreshed {
                account_id,
                result,
                manual,
            } => self.on_account_refreshed(account_id, result, manual),
            Internal::HostsProbed(result) => self.on_hosts_probed(result),
            Internal::ModeApplied(generation, mode, result) => {
                self.on_mode_applied(generation, mode, result)
            }
            Internal::CoreMode(generation, result) => self.on_core_mode(generation, result),
            Internal::GeoUpdated(result) => self.on_geo_updated(result),
            Internal::DiagnosticsExported(result) => self.on_diagnostics_exported(result),
        }
    }
}
