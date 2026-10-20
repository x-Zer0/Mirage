//! Window callbacks grouped by feature; each only queues commands or updates UI state.
use super::{Command, MainWindow, SHARED, lifecycle::send_mode, send};
use crate::app::settings::SettingsInput;
use crate::domain::redact::{Password, SecretString};
use slint::ComponentHandle;
use tokio::sync::mpsc;

type Commands = mpsc::Sender<Command>;

pub(super) fn wire(window: &MainWindow, commands: Commands) {
    wire_connection(window, &commands);
    wire_profiles(window, &commands);
    wire_nodes(window, &commands);
    wire_accounts(window, &commands);
    wire_settings(window, &commands);
    wire_logs(window, &commands);
}

fn wire_connection(window: &MainWindow, commands: &Commands) {
    let c = commands.clone();
    window.on_connect(move || send(&c, Command::Connect));
    let c = commands.clone();
    window.on_disconnect(move || send(&c, Command::Disconnect));
    let c = commands.clone();
    window.on_set_system_proxy(move |on| send(&c, Command::SetSystemProxy(on)));
    let c = commands.clone();
    window.on_set_tun(move |on| send(&c, Command::SetTun(on)));
    let c = commands.clone();
    window.on_manage_service(move |install| send(&c, Command::ManageService(install)));
    let c = commands.clone();
    window.on_set_routing_mode(move |i| send_mode(&c, i));
    let c = commands.clone();
    window.on_check_connectivity(move || send(&c, Command::CheckConnectivity));
}

fn wire_profiles(window: &MainWindow, commands: &Commands) {
    let c = commands.clone();
    window.on_import_file(move || send(&c, Command::PickAndImport));
    let c = commands.clone();
    window.on_add_remote(move |name, url| {
        send(
            &c,
            Command::AddRemoteProfile {
                name: name.into(),
                url: SecretString::new(url.as_str()),
            },
        )
    });
    let c = commands.clone();
    window.on_update_profile(move |id| send(&c, Command::UpdateProfile(id.into())));
    let c = commands.clone();
    window.on_update_all(move || send(&c, Command::UpdateAll));
    let c = commands.clone();
    window.on_select_profile(move |id| send(&c, Command::SelectProfile(id.into())));
    let c = commands.clone();
    window.on_remove_profile(move |id| send(&c, Command::RemoveProfile(id.into())));
    let c = commands.clone();
    window.on_copy_profile_url(move |id| send(&c, Command::CopyProfileUrl(id.into())));
    let c = commands.clone();
    window.on_set_update_policy(move |id, hours, egress| {
        send(
            &c,
            Command::SetUpdatePolicy {
                id: id.into(),
                interval_hours: u32::try_from(hours).unwrap_or(0),
                egress: usize::try_from(egress).unwrap_or(0),
            },
        )
    });
}

fn wire_nodes(window: &MainWindow, commands: &Commands) {
    let c = commands.clone();
    window.on_select_node(move |group, node| {
        send(
            &c,
            Command::SelectNode {
                group: group.into(),
                node: node.into(),
            },
        )
    });
    let c = commands.clone();
    window.on_test_delay(move |group| send(&c, Command::TestGroupDelay(group.into())));
    let w = window.as_weak();
    window.on_sort_nodes(move |on| {
        let Some(w) = w.upgrade() else { return };
        SHARED.with_borrow_mut(|s| {
            s.applied.list.sort_by_delay = on;
            s.applied.relist(&w);
        });
    });
    let w = window.as_weak();
    window.on_filter_nodes(move |filter| {
        let Some(w) = w.upgrade() else { return };
        SHARED.with_borrow_mut(|s| {
            s.applied.list.filter = filter.trim().to_lowercase();
            s.applied.relist(&w);
        });
    });
}

fn wire_accounts(window: &MainWindow, commands: &Commands) {
    let c = commands.clone();
    window.on_provider_login(move |provider, email, password| {
        // The Slint string is dropped right after; the core zeroes its copy.
        send(
            &c,
            Command::ProviderLogin {
                provider_id: provider.into(),
                email: email.into(),
                password: Password::new(password.as_str().to_owned()),
            },
        )
    });
    let c = commands.clone();
    window.on_revalidate_account(move |id| send(&c, Command::RevalidateAccount(id.into())));
    let c = commands.clone();
    window.on_reveal_account(move |id, visible| {
        send(
            &c,
            Command::RevealAccount {
                id: id.into(),
                visible,
            },
        );
    });
    let c = commands.clone();
    window
        .on_import_legacy(move |provider| send(&c, Command::ImportLegacyProvider(provider.into())));
    let c = commands.clone();
    window.on_probe_hosts(move || send(&c, Command::ProbeProviderHosts));
}

fn wire_settings(window: &MainWindow, commands: &Commands) {
    let c = commands.clone();
    window.on_save_settings(
        move |port,
              bypass,
              ua,
              allow_lan,
              auto_connect,
              close_to_tray,
              autostart,
              connection_log_limit,
              operation_log_days| {
            send(
                &c,
                Command::SaveSettings(SettingsInput {
                    mixed_port: port.into(),
                    bypass: bypass.into(),
                    default_user_agent: ua.into(),
                    allow_lan,
                    auto_connect,
                    close_to_tray,
                    autostart,
                    connection_log_limit: connection_log_limit.into(),
                    operation_log_days: operation_log_days.into(),
                }),
            )
        },
    );
    let c = commands.clone();
    window.on_update_geodata(move || send(&c, Command::UpdateGeodata));
}

fn wire_logs(window: &MainWindow, commands: &Commands) {
    let c = commands.clone();
    window.on_export_diagnostics(move || send(&c, Command::ExportDiagnostics));
    let c = commands.clone();
    window.on_retry_error(move || send(&c, Command::RetryNotice));
    let c = commands.clone();
    window.on_dismiss_error(move || send(&c, Command::DismissNotice));
    let c = commands.clone();
    window.on_clear_connection_logs(move || send(&c, Command::ClearConnectionLogs));
    let c = commands.clone();
    window.on_filter_logs(move |active, operations, text, filter, paused| {
        send(
            &c,
            Command::QueryLogs(crate::app::logs::LogQuery {
                active,
                operations,
                text: text.into(),
                filter: filter.max(0) as usize,
                paused,
                anchor: None,
            }),
        );
    });
}
