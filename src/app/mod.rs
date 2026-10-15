//! Application core actor (DESIGN.md §6.5). One task owns all mutable state;
//! slow work runs in spawned tasks that report back through `Internal`
//! messages tagged with the generation they were started for.

mod pipeline;
mod profiles;
mod runtime;
pub mod state;

use crate::cores::mihomo::binary::{self, CoreBinary};
use crate::cores::mihomo::controller::ProxyGroupView;
use crate::domain::error::AppError;
use crate::storage::layout::Layout;
use runtime::Running;
use state::{AppStateView, Notice, RuntimeStatus};
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::{mpsc, watch};

const COMMAND_QUEUE: usize = 64;
/// DESIGN.md §2: avoids 7890/7897 and ziyoumao's 17890.
pub const DEFAULT_MIXED_PORT: u16 = 17896;

#[derive(Debug)]
pub enum Command {
    PickAndImport,
    /// From `--import <path>`; queued until the core binary is verified.
    ImportLocalFile(std::path::PathBuf),
    SelectProfile(String),
    RemoveProfile(String),
    Connect,
    Disconnect,
    SelectNode {
        group: String,
        node: String,
    },
    TestGroupDelay(String),
    RefreshGroups,
    DismissNotice,
    Quit,
}

enum Internal {
    CoreVerified(Result<CoreBinary, AppError>),
    Imported(Result<pipeline::ImportOutcome, AppError>),
    Started(u64, Result<Running, AppError>),
    CoreExited { generation: u64, detail: String },
    Groups(u64, Result<Vec<ProxyGroupView>, AppError>),
    Stopped(u64),
    DelayDone(u64, String, Result<(), AppError>),
}

pub struct CoreHandle {
    tx: mpsc::Sender<Command>,
    state: watch::Receiver<Arc<AppStateView>>,
    thread: JoinHandle<()>,
}

impl CoreHandle {
    pub fn sender(&self) -> mpsc::Sender<Command> {
        self.tx.clone()
    }

    pub fn state(&self) -> watch::Receiver<Arc<AppStateView>> {
        self.state.clone()
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        RUNTIME.get().expect("runtime started").clone()
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

pub fn spawn(layout: Layout) -> std::io::Result<CoreHandle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("mirage-rt")
        .enable_all()
        .build()?;
    let _ = RUNTIME.set(runtime.handle().clone());
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
    let (state_tx, state_rx) = watch::channel(Arc::new(AppStateView::default()));
    let thread = std::thread::Builder::new()
        .name("mirage-core".into())
        .spawn(move || runtime.block_on(AppCore::new(layout, state_tx).run(rx)))?;
    Ok(CoreHandle {
        tx,
        state: state_rx,
        thread,
    })
}

struct AppCore {
    layout: Layout,
    state_tx: watch::Sender<Arc<AppStateView>>,
    view: AppStateView,
    binary: Option<CoreBinary>,
    profiles: profiles::Profiles,
    running: Option<Running>,
    generation: u64,
    /// Commands that need a verified core, held until verification finishes.
    deferred: Vec<Command>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: Option<mpsc::UnboundedReceiver<Internal>>,
}

impl AppCore {
    fn new(layout: Layout, state_tx: watch::Sender<Arc<AppStateView>>) -> Self {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        let mut view = AppStateView {
            mixed_port: DEFAULT_MIXED_PORT,
            core_version: "检查中…".into(),
            ..Default::default()
        };
        let profiles = profiles::Profiles::load(&layout).unwrap_or_else(|e| {
            view.notice = Some(error_notice(&e));
            profiles::Profiles::empty()
        });
        Self {
            layout,
            state_tx,
            view,
            binary: None,
            profiles,
            running: None,
            generation: 0,
            deferred: Vec::new(),
            internal_tx,
            internal_rx: Some(internal_rx),
        }
    }

    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut internal = self.internal_rx.take().expect("internal receiver");
        self.startup();
        loop {
            tokio::select! {
                cmd = commands.recv() => match cmd {
                    Some(Command::Quit) | None => break,
                    Some(cmd) => self.handle(cmd),
                },
                Some(msg) = internal.recv() => self.handle_internal(msg),
            }
            self.publish();
        }
        if let Some(running) = self.running.take() {
            runtime::stop(running, &self.layout).await;
        }
        tracing::debug!("app core stopped");
    }

    fn startup(&mut self) {
        clean_check_dir(&self.layout);
        runtime::remove_run_config(&self.layout);
        if let Err(e) = binary::seed_core_home(&self.layout) {
            self.view.notice = Some(error_notice(&e));
        }
        let layout = self.layout.clone();
        self.spawn(async move { Internal::CoreVerified(binary::verify(&layout).await) });
        self.refresh_profiles_view();
        self.publish();
    }

    fn spawn(&self, fut: impl Future<Output = Internal> + Send + 'static) {
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            // The actor only drops the receiver while shutting down.
            let _ = tx.send(fut.await);
        });
    }

    fn publish(&self) {
        self.state_tx.send_replace(Arc::new(self.view.clone()));
    }
}

mod handlers;

fn error_notice(e: &AppError) -> Notice {
    tracing::warn!(error = %e, "operation failed");
    Notice {
        error: true,
        text: e.user_message(),
    }
}

fn info_notice(text: impl Into<String>) -> Notice {
    Notice {
        error: false,
        text: text.into(),
    }
}

/// Removes leftover check files; they may hold node credentials (§11.5).
fn clean_check_dir(layout: &Layout) {
    let Ok(entries) = std::fs::read_dir(layout.core_check_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

#[allow(dead_code)] // referenced by the UI bridge for status text
pub fn status_text(status: &RuntimeStatus) -> String {
    match status {
        RuntimeStatus::Stopped => "未连接".into(),
        RuntimeStatus::Starting => "正在连接…".into(),
        RuntimeStatus::Running => "已连接".into(),
        RuntimeStatus::Stopping => "正在断开…".into(),
        RuntimeStatus::Failed(reason) => format!("连接失败：{reason}"),
    }
}
