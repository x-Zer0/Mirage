//! Glue between Slint components and the app core (DESIGN.md §7).
//! UI callbacks only enqueue commands; they never await or block. State
//! snapshots are throttled to one UI update per 100 ms; traffic samples and
//! log batches arrive through [`LiveSink`] and only while the window is shown.

mod models;

use crate::app::live::{LiveSink, UiLogLine};
use crate::app::settings::SettingsInput;
use crate::app::state::{AppStateView, Notice, TrafficSample};
use crate::app::{Command, CoreHandle};
use crate::domain::model::RoutingMode;
use crate::domain::redact::{Password, SecretString};
use crate::platform::windows::tray_balloon;
use crate::storage::layout::Layout;
use slint::{ComponentHandle, Model, VecModel};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

slint::include_modules!();

const STATE_THROTTLE: Duration = Duration::from_millis(100);
/// §18: the UI keeps the most recent 2000 core log lines.
const MAX_LOG_LINES: usize = 2000;

pub struct Ui {
    window: MainWindow,
    tray: AppTray,
}

impl Ui {
    pub fn build(
        layout: &Layout,
        version: &str,
        core: &CoreHandle,
        start_hidden: bool,
    ) -> Result<Self, slint::PlatformError> {
        let window = MainWindow::new()?;
        window.set_version(version.into());
        window.set_data_dir(layout.root.display().to_string().into());
        window.set_logs(Rc::new(VecModel::<LogRow>::default()).into());

        let tray = AppTray::new()?;
        let commands = core.sender();
        wire_window(&window, commands.clone());
        wire_tray(&window, &tray, commands.clone());
        wire_close(&window, &tray, commands.clone());
        forward_state(&core.runtime(), core.state(), &window, &tray);
        send(
            &commands,
            Command::SetLiveSink(Arc::new(UiSink {
                window: window.as_weak(),
            })),
        );

        tray.show()?;
        if !start_hidden {
            window.show()?;
        }
        tray.set_window_visible(!start_hidden);
        send(&commands, Command::WindowVisible(!start_hidden));
        Ok(Self { window, tray })
    }

    /// Callback for the single-instance listener: shows the window from any thread.
    pub fn activator(&self, commands: mpsc::Sender<Command>) -> impl Fn() + Send + 'static {
        let window = self.window.as_weak();
        let tray = self.tray.as_weak();
        move || {
            let tray = tray.clone();
            let commands = commands.clone();
            let posted = window.upgrade_in_event_loop(move |w| {
                if let Some(t) = tray.upgrade() {
                    show_window(&w, &t, &commands);
                }
            });
            if posted.is_err() {
                tracing::debug!("activation ignored: event loop already stopped");
            }
        }
    }
}

/// UI-thread state shared by the forwarding and close callbacks.
#[derive(Default)]
struct Shared {
    applied: models::Applied,
    /// Last notice already shown as toast or balloon (by sequence number).
    shown_seq: u64,
    close_to_tray: bool,
    close_hint_shown: bool,
}

fn send(commands: &mpsc::Sender<Command>, cmd: Command) {
    if let Err(e) = commands.try_send(cmd) {
        tracing::warn!(error = %e, "command dropped");
    }
}

fn wire_window(window: &MainWindow, commands: mpsc::Sender<Command>) {
    let c = commands.clone();
    window.on_connect(move || send(&c, Command::Connect));
    let c = commands.clone();
    window.on_disconnect(move || send(&c, Command::Disconnect));
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
    let c = commands.clone();
    window.on_refresh_groups(move || send(&c, Command::RefreshGroups));
    let w = window.as_weak();
    window.on_sort_nodes(move |on| {
        let Some(w) = w.upgrade() else { return };
        SHARED.with_borrow_mut(|s| {
            s.applied.list.sort_by_delay = on;
            s.applied.relist(&w);
        });
    });
    let c = commands.clone();
    window.on_set_system_proxy(move |on| send(&c, Command::SetSystemProxy(on)));
    let c = commands.clone();
    window.on_set_routing_mode(move |i| send_mode(&c, i));
    let c = commands.clone();
    window.on_check_connectivity(move || send(&c, Command::CheckConnectivity));
    let c = commands.clone();
    window.on_provider_login(move |email, password| {
        // The Slint string is dropped right after; the core zeroes its copy.
        send(
            &c,
            Command::ProviderLogin {
                email: email.into(),
                password: Password::new(password.as_str().to_owned()),
            },
        )
    });
    let c = commands.clone();
    window.on_revalidate_account(move |id| send(&c, Command::RevalidateAccount(id.into())));
    let c = commands.clone();
    window.on_remove_account(move |id, with_profile| {
        send(&c, Command::RemoveAccount(id.into(), with_profile))
    });
    let c = commands.clone();
    window.on_import_legacy(move || send(&c, Command::ImportLegacyZiyoumao));
    let c = commands.clone();
    window.on_probe_hosts(move || send(&c, Command::ProbeProviderHosts));
    let c = commands.clone();
    window.on_save_settings(
        move |port, bypass, ua, egress, auto_connect, close_to_tray, autostart| {
            send(
                &c,
                Command::SaveSettings(SettingsInput {
                    mixed_port: port.into(),
                    bypass: bypass.into(),
                    default_user_agent: ua.into(),
                    provider_egress: usize::try_from(egress).unwrap_or(0),
                    auto_connect,
                    close_to_tray,
                    autostart,
                }),
            )
        },
    );
    let c = commands.clone();
    window.on_update_geodata(move || send(&c, Command::UpdateGeodata));
    let c = commands.clone();
    window.on_export_diagnostics(move || send(&c, Command::ExportDiagnostics));
    let c = commands.clone();
    window.on_retry_error(move || send(&c, Command::RetryNotice));
    window.on_dismiss_error(move || send(&commands, Command::DismissNotice));
    let w = window.as_weak();
    window.on_clear_logs(move || {
        if let Some(w) = w.upgrade() {
            w.set_logs(Rc::new(VecModel::<LogRow>::default()).into());
        }
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

fn send_mode(commands: &mpsc::Sender<Command>, index: i32) {
    if let Some(mode) = usize::try_from(index)
        .ok()
        .and_then(|i| RoutingMode::ALL.get(i))
    {
        send(commands, Command::SetRoutingMode(*mode));
    }
}

fn wire_tray(window: &MainWindow, tray: &AppTray, commands: mpsc::Sender<Command>) {
    let w = window.as_weak();
    let t = tray.as_weak();
    let c = commands.clone();
    tray.on_toggle_window(move || {
        let (Some(w), Some(t)) = (w.upgrade(), t.upgrade()) else {
            return;
        };
        if w.window().is_visible() {
            hide_window(&w, &t, &c);
        } else {
            show_window(&w, &t, &c);
        }
    });

    let c = commands.clone();
    let t = tray.as_weak();
    tray.on_toggle_connection(move || {
        let running = t.upgrade().is_some_and(|t| t.get_running());
        send(
            &c,
            if running {
                Command::Disconnect
            } else {
                Command::Connect
            },
        );
    });

    let c = commands.clone();
    tray.on_set_system_proxy(move |on| send(&c, Command::SetSystemProxy(on)));
    let c = commands.clone();
    tray.on_set_routing_mode(move |i| send_mode(&c, i));

    tray.on_quit_app(move || quit(&commands));
}

fn quit(commands: &mpsc::Sender<Command>) {
    // The core stops after the event loop returns (main.rs); quitting the
    // loop first keeps the UI responsive while the core shuts down.
    let _ = commands.try_send(Command::Quit);
    let _ = slint::quit_event_loop();
}

/// Closing the window hides it to the tray (§17.2), with a one-time hint;
/// with "close to tray" off it quits instead.
fn wire_close(window: &MainWindow, tray: &AppTray, commands: mpsc::Sender<Command>) {
    let t = tray.as_weak();
    window.window().on_close_requested(move || {
        let (to_tray, hint_shown) = SHARED.with_borrow(|s| (s.close_to_tray, s.close_hint_shown));
        if !to_tray {
            quit(&commands);
            return slint::CloseRequestResponse::HideWindow;
        }
        if let Some(t) = t.upgrade() {
            t.set_window_visible(false);
        }
        send(&commands, Command::WindowVisible(false));
        if !hint_shown {
            SHARED.with_borrow_mut(|s| s.close_hint_shown = true);
            tray_balloon::show(
                "Mirage 仍在运行",
                "窗口已最小化到托盘。右键托盘图标可断开或退出，可在设置中改为关闭即退出。",
                false,
            );
            send(&commands, Command::CloseHintShown);
        }
        slint::CloseRequestResponse::HideWindow
    });
}

/// Pushes state snapshots to the UI thread, at most once per throttle window.
fn forward_state(
    rt: &tokio::runtime::Handle,
    mut state: watch::Receiver<Arc<AppStateView>>,
    window: &MainWindow,
    tray: &AppTray,
) {
    let window = window.as_weak();
    let tray = tray.as_weak();
    rt.spawn(async move {
        loop {
            let snapshot = state.borrow_and_update().clone();
            let tray = tray.clone();
            let posted = window.upgrade_in_event_loop(move |w| {
                SHARED.with_borrow_mut(|shared| {
                    models::apply(&w, &snapshot, &mut shared.applied);
                    if let Some(t) = tray.upgrade() {
                        models::apply_tray(&t, &snapshot);
                    }
                    show_notices(&w, &snapshot, shared);
                });
            });
            if posted.is_err() || state.changed().await.is_err() {
                break;
            }
            tokio::time::sleep(STATE_THROTTLE).await;
        }
    });
}

/// New toasts, tray balloons for events that matter while hidden, and the
/// close behaviour from settings.
fn show_notices(w: &MainWindow, s: &AppStateView, shared: &mut Shared) {
    shared.close_to_tray = s.settings.close_to_tray;
    shared.close_hint_shown |= s.settings.close_hint_shown;
    let newest = [s.toast.as_ref(), s.error.as_ref()]
        .into_iter()
        .flatten()
        .filter(|n| n.seq > shared.shown_seq)
        .collect::<Vec<&Notice>>();
    for n in newest {
        shared.shown_seq = shared.shown_seq.max(n.seq);
        if !n.error {
            w.invoke_show_toast(n.text.clone().into());
        }
        if n.notify && !w.window().is_visible() {
            tray_balloon::show(
                if n.error { "Mirage 出错" } else { "Mirage" },
                &n.text,
                n.error,
            );
        }
    }
}

fn show_window(w: &MainWindow, t: &AppTray, commands: &mpsc::Sender<Command>) {
    if let Err(e) = w.show() {
        tracing::warn!(error = %e, "show window failed");
        return;
    }
    w.window().set_minimized(false);
    t.set_window_visible(true);
    send(commands, Command::WindowVisible(true));
}

fn hide_window(w: &MainWindow, t: &AppTray, commands: &mpsc::Sender<Command>) {
    let _ = w.hide();
    t.set_window_visible(false);
    send(commands, Command::WindowVisible(false));
}

/// Receives live data on runtime threads and posts it to the UI thread.
struct UiSink {
    window: slint::Weak<MainWindow>,
}

impl std::fmt::Debug for UiSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UiSink")
    }
}

// UI-thread only: written from closures posted with `upgrade_in_event_loop`
// and from Slint callbacks, both of which run on the event loop thread.
thread_local! {
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
    static TRAFFIC: RefCell<VecDeque<TrafficSample>> = RefCell::new(VecDeque::with_capacity(models::HISTORY));
}

impl LiveSink for UiSink {
    fn traffic(&self, sample: TrafficSample) {
        let _ = self.window.upgrade_in_event_loop(move |w| {
            TRAFFIC.with_borrow_mut(|h| {
                if h.len() == models::HISTORY {
                    h.pop_front();
                }
                h.push_back(sample);
                models::apply_traffic(&w, h);
            });
        });
    }

    fn log_lines(&self, lines: Vec<UiLogLine>) {
        let _ = self.window.upgrade_in_event_loop(move |w| {
            let model = w.get_logs();
            let Some(vec) = model.as_any().downcast_ref::<VecModel<LogRow>>() else {
                return;
            };
            let rows = lines.into_iter().map(|line| LogRow {
                level: line.level.into(),
                text: line.text.into(),
            });
            let excess = (vec.row_count() + rows.len()).saturating_sub(MAX_LOG_LINES);
            if excess == 0 {
                vec.extend(rows);
            } else {
                // Full ring: keep the newest lines in one model reset.
                let kept: Vec<LogRow> = vec.iter().skip(excess).chain(rows).collect();
                vec.set_vec(kept);
            }
        });
    }
}
