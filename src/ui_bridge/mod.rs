//! Glue between Slint components and the app core (DESIGN.md §7).
//! UI callbacks only enqueue commands; they never await or block. State
//! snapshots are throttled to one UI update per 100 ms.

mod models;

use crate::app::state::AppStateView;
use crate::app::{Command, CoreHandle};
use crate::storage::layout::Layout;
use slint::ComponentHandle;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

slint::include_modules!();

const STATE_THROTTLE: Duration = Duration::from_millis(100);

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

        let tray = AppTray::new()?;
        let commands = core.sender();
        wire_window(&window, commands.clone());
        wire_tray(&window, &tray, commands);
        wire_close_to_tray(&window, &tray);
        forward_state(&core.runtime(), core.state(), &window, &tray);

        tray.show()?;
        if !start_hidden {
            window.show()?;
        }
        tray.set_window_visible(!start_hidden);
        Ok(Self { window, tray })
    }

    /// Callback for the single-instance listener: shows the window from any thread.
    pub fn activator(&self) -> impl Fn() + Send + 'static {
        let window = self.window.as_weak();
        let tray = self.tray.as_weak();
        move || {
            let tray = tray.clone();
            let posted = window.upgrade_in_event_loop(move |w| {
                show_window(&w);
                if let Some(t) = tray.upgrade() {
                    t.set_window_visible(true);
                }
            });
            if posted.is_err() {
                tracing::debug!("activation ignored: event loop already stopped");
            }
        }
    }
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
    window.on_select_profile(move |id| send(&c, Command::SelectProfile(id.into())));
    let c = commands.clone();
    window.on_remove_profile(move |id| send(&c, Command::RemoveProfile(id.into())));
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
    window.on_dismiss_notice(move || send(&commands, Command::DismissNotice));
    window.on_node_matches(|name, filter| {
        filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
    });
}

fn wire_tray(window: &MainWindow, tray: &AppTray, commands: mpsc::Sender<Command>) {
    let w = window.as_weak();
    let t = tray.as_weak();
    tray.on_toggle_window(move || {
        let (Some(w), Some(t)) = (w.upgrade(), t.upgrade()) else {
            return;
        };
        let visible = w.window().is_visible();
        if visible {
            let _ = w.hide();
        } else {
            show_window(&w);
        }
        t.set_window_visible(!visible);
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

    tray.on_quit_app(move || {
        // The core stops after the event loop returns (main.rs); quitting the
        // loop first keeps the UI responsive while the core shuts down.
        let _ = commands.try_send(Command::Quit);
        let _ = slint::quit_event_loop();
    });
}

/// Closing the window hides it; the tray keeps the event loop alive (§17.2).
fn wire_close_to_tray(window: &MainWindow, tray: &AppTray) {
    let t = tray.as_weak();
    window.window().on_close_requested(move || {
        if let Some(t) = t.upgrade() {
            t.set_window_visible(false);
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
                models::apply(&w, &snapshot);
                if let Some(t) = tray.upgrade() {
                    models::apply_tray(&t, &snapshot);
                }
            });
            if posted.is_err() || state.changed().await.is_err() {
                break;
            }
            tokio::time::sleep(STATE_THROTTLE).await;
        }
    });
}

fn show_window(w: &MainWindow) {
    if let Err(e) = w.show() {
        tracing::warn!(error = %e, "show window failed");
        return;
    }
    w.window().set_minimized(false);
}
