//! Glue between Slint components and the app core (DESIGN.md §7).
//! UI callbacks only enqueue commands; they never await or block.

use crate::app::Command;
use crate::storage::layout::Layout;
use slint::ComponentHandle;
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
        commands: mpsc::Sender<Command>,
        start_hidden: bool,
    ) -> Result<Self, slint::PlatformError> {
        let window = MainWindow::new()?;
        window.set_version(version.into());
        window.set_data_dir(layout.root.display().to_string().into());

        let tray = AppTray::new()?;
        wire_tray(&window, &tray, commands);
        wire_close_to_tray(&window, &tray);

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

    tray.on_quit_app(move || {
        // The core shuts down after the event loop returns (main.rs); the
        // command lets it start cleanup early. A full queue is not fatal here.
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

fn show_window(w: &MainWindow) {
    if let Err(e) = w.show() {
        tracing::warn!(error = %e, "show window failed");
        return;
    }
    w.window().set_minimized(false);
}
