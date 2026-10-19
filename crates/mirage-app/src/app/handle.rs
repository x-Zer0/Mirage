//! Runtime thread, command channel and application shutdown handle.
use super::{
    AppCore, Command,
    state::{AppStateView, Notice},
};
use crate::storage::{
    layout::Layout,
    meta::{self, SettingsFile, UiGeometry},
};
use std::{sync::Arc, thread::JoinHandle, time::Duration};
use tokio::sync::{mpsc, watch};

const COMMAND_QUEUE: usize = 64;
/// Upper bound for the logoff/shutdown cleanup (§11.5).
const SESSION_END_WAIT: Duration = Duration::from_secs(5);

pub struct CoreHandle {
    initial_ui_geometry: UiGeometry,
    tx: mpsc::Sender<Command>,
    state: watch::Receiver<Arc<AppStateView>>,
    thread: JoinHandle<()>,
}

impl CoreHandle {
    pub fn initial_ui_geometry(&self) -> UiGeometry {
        self.initial_ui_geometry
    }

    pub fn sender(&self) -> mpsc::Sender<Command> {
        self.tx.clone()
    }

    pub fn state(&self) -> watch::Receiver<Arc<AppStateView>> {
        self.state.clone()
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        RUNTIME.get().expect("runtime started").clone()
    }

    /// Hook for `platform::session`: asks the actor to restore the system proxy
    /// and stop the core, then waits (bounded) so cleanup finishes before
    /// Windows ends the process.
    pub fn session_end_hook(&self) -> impl Fn() + Send + Sync + 'static {
        let tx = self.tx.clone();
        move || {
            let (ack_tx, ack_rx) = std::sync::mpsc::channel();
            if tx.blocking_send(Command::SessionEnding(ack_tx)).is_ok()
                && ack_rx.recv_timeout(SESSION_END_WAIT).is_ok()
            {
                return;
            }
            tracing::warn!("session-end cleanup did not finish in time");
        }
    }

    /// Stops the core (if running) and waits for the runtime thread.
    pub fn shutdown(self) {
        // A closed channel means the actor already stopped; nothing left to do.
        let _ = self.tx.blocking_send(Command::Quit);
        if self.thread.join().is_err() {
            tracing::error!("app runtime thread panicked");
        }
    }
}

static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

/// `startup_notice` carries the outcome of [`recovery::run`] to the UI.
pub fn spawn(layout: Layout, startup_notice: Option<String>) -> std::io::Result<CoreHandle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("mirage-rt")
        .enable_all()
        .build()?;
    let _ = RUNTIME.set(runtime.handle().clone());
    // Read once, synchronously, before the UI is constructed or shown.
    let settings = meta::load::<SettingsFile>(&layout.settings_file(), "app.settings");
    let initial_ui_geometry = settings
        .as_ref()
        .map(|s| s.ui_geometry)
        .unwrap_or_default()
        .normalized();
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
    let (state_tx, state_rx) = watch::channel(Arc::new(AppStateView::default()));
    let thread = std::thread::Builder::new()
        .name("mirage-core".into())
        .spawn(move || {
            let mut core = AppCore::new(layout, state_tx, settings);
            if let Some(text) = startup_notice {
                core.view.notice = Some(Notice::error(text).with_stage("启动恢复"));
            }
            runtime.block_on(core.run(rx));
            runtime.shutdown_timeout(Duration::from_secs(2));
        })?;
    Ok(CoreHandle {
        initial_ui_geometry,
        tx,
        state: state_rx,
        thread,
    })
}
