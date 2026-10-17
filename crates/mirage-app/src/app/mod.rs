//! Application actor and its command/state boundary.
mod accounts;
mod actor;
mod connectivity;
pub mod diagnostics;
mod diagnostics_actions;
mod handle;
mod handlers;
pub mod live;
mod messages;
mod nodes;
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
    binary: Option<CoreBinary>,
    profiles: profiles::Profiles,
    running: Option<Running>,
    generation: u64,
    connectivity_request: u64,
    connectivity_task: Option<tokio::task::JoinHandle<()>>,
    settings: SettingsFile,
    /// Consecutive supervised restarts since the core last ran stably (§5.2).
    restart_attempts: u32,
    proxy_before_tun: Option<bool>,
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
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: Option<mpsc::UnboundedReceiver<Internal>>,
}

fn error_notice(e: &AppError) -> Notice {
    Notice::from_error(e)
}

fn info_notice(text: impl Into<String>) -> Notice {
    Notice::info(text)
}
