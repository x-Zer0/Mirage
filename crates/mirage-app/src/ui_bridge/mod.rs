//! Slint UI construction and shared UI-thread state.
mod callbacks;
mod lifecycle;
mod models;
mod updates;
mod window;

use crate::app::{Command, CoreHandle};
use crate::storage::layout::Layout;
use slint::ComponentHandle;
use std::{cell::RefCell, sync::Arc};
use tokio::sync::mpsc;

slint::include_modules!();

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
        window::wire_restore(&window);
        callbacks::wire(&window, commands.clone());
        lifecycle::wire_tray(&window, &tray, commands.clone());
        lifecycle::wire_close(&window, &tray, commands.clone());
        updates::forward_state(&core.runtime(), core.state(), &window, &tray);
        send(
            &commands,
            Command::SetLiveSink(Arc::new(updates::UiSink {
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
                    lifecycle::show_window(&w, &t, &commands);
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

// Slint callbacks and posted state updates all run on the UI thread.
thread_local! {
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
}
