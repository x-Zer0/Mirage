//! Application actor and its command/state boundary.
mod accounts;
mod actor;
mod connectivity;
mod delays;
pub mod diagnostics;
mod diagnostics_actions;
mod handle;
mod handlers;
mod hosts;
pub mod live;
mod log_actions;
pub mod logs;
mod messages;
mod nodes;
mod overlay;
mod pipeline;
mod profile_actions;
mod profile_view;
mod profiles;
pub mod recovery;
mod reload;
mod runtime;
mod session;
pub mod settings;
pub mod state;
mod supervise;
pub mod sysproxy;
mod tun;
mod updates;

use crate::cores::mihomo::binary::CoreBinary;
use crate::domain::{
    error::AppError,
    model::{AccountId, AccountStatus, ProfileId, RevisionId},
};
use crate::platform::windows::secret::SecretStore;
use crate::storage::{layout::Layout, meta::SettingsFile};
pub use handle::{CoreHandle, spawn};
pub use messages::Command;
use messages::Internal;
use runtime::Running;
pub use state::status_text;
use state::{AppStateView, Notice};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{Semaphore, mpsc, watch};

/// DESIGN.md §2: avoids 7890/7897 and ziyoumao's 17890.
pub const DEFAULT_MIXED_PORT: u16 = 17896;

struct AppCore {
    layout: Layout,
    state_tx: watch::Sender<Arc<AppStateView>>,
    view: AppStateView,
    view_dirty: bool,
    binary: Option<CoreBinary>,
    profiles: profiles::Profiles,
    running: Option<Running>,
    generation: u64,
    delay_tests: delays::DelayTests,
    connectivity_request: u64,
    connectivity_task: Option<tokio::task::JoinHandle<()>>,
    settings: SettingsFile,
    /// Pending UI geometry save, deferred until resizing settles.
    ui_geometry_save_at: Option<tokio::time::Instant>,
    /// Consecutive supervised restarts since the core last ran stably (§5.2).
    restart_attempts: u32,
    connection_source: &'static str,
    connected_since: Option<std::time::Instant>,
    /// Commands that need a verified core, held until verification finishes.
    deferred: Vec<Command>,
    secrets: SecretStore,
    /// Profiles with a download/check in flight.
    updating: HashSet<ProfileId>,
    fetch_permits: Arc<Semaphore>,
    /// Candidate that arrived during startup or a hot reload (§6.5).
    queued_reload: Option<(ProfileId, RevisionId)>,
    accounts: accounts::Accounts,
    /// Status before a running session check, restored on inconclusive errors.
    validating_prev: HashMap<AccountId, AccountStatus>,
    live: live::LiveStreams,
    operation_logs: crate::storage::operation_logs::OperationLogs,
    connection_logs: logs::ConnectionHistory,
    log_query: logs::LogQuery,
    update_operations: HashMap<ProfileId, String>,
    /// `overrides/hosts.txt` as saved (iteration 2 §2).
    hosts: crate::overrides::hosts::Hosts,
    /// Overlay whose hot reload just failed; not retried until the settings change.
    failed_overlay: Option<crate::cores::mihomo::compose::UserOverlay>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: Option<mpsc::UnboundedReceiver<Internal>>,
}

fn error_notice(e: &AppError) -> Notice {
    Notice::from_error(e)
}

fn info_notice(text: impl Into<String>) -> Notice {
    Notice::info(text)
}

/// Format bytes as GiB with one decimal place.
fn format_gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}
