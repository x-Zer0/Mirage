//! Application core actor (DESIGN.md §6.5). One task owns all mutable state;
//! slow work runs in spawned tasks that report back through `Internal`
//! messages tagged with the generation they were started for.

mod accounts;
mod connectivity;
pub mod diagnostics;
pub mod live;
mod pipeline;
mod profiles;
pub mod recovery;
mod runtime;
pub mod settings;
pub mod state;
mod supervise;
pub mod sysproxy;
mod tun;
mod updates;

use crate::cores::mihomo::binary::{self, CoreBinary};
use crate::cores::mihomo::controller::ProxyGroupView;
use crate::domain::error::AppError;
use crate::domain::model::{AccountId, AccountStatus, ProfileId, RevisionId, RoutingMode};
use crate::domain::redact::{Password, SecretString};
use crate::platform::windows::secret::SecretStore;
use crate::storage::layout::Layout;
use crate::storage::meta::{self, SettingsFile};
use runtime::Running;
use state::{AppStateView, Notice, RuntimeStatus};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc, watch};

const COMMAND_QUEUE: usize = 64;
/// How often an applied system proxy is compared with WinINet (§11.3).
const SYSPROXY_WATCH: Duration = Duration::from_secs(5);
/// Upper bound for the logoff/shutdown cleanup (§11.5).
const SESSION_END_WAIT: Duration = Duration::from_secs(5);
/// DESIGN.md §2: avoids 7890/7897 and ziyoumao's 17890.
pub const DEFAULT_MIXED_PORT: u16 = 17896;

#[derive(Debug)]
pub enum Command {
    PickAndImport,
    /// From `--import <path>`; queued until the core binary is verified.
    ImportLocalFile(std::path::PathBuf),
    SelectProfile(String),
    RemoveProfile(String),
    /// Name may be empty (then content-disposition or the host is used).
    AddRemoteProfile {
        name: String,
        url: SecretString,
    },
    UpdateProfile(String),
    UpdateAll,
    /// ziyoumao login; the password is zeroed after the request (§6.1).
    ProviderLogin {
        email: String,
        password: Password,
    },
    RevalidateAccount(AccountId),
    /// Local-only removal; `bool`: also remove the account's profile.
    RemoveAccount(AccountId, bool),
    /// Read-only import of the original client's config (ZIYOUMAO.md §9).
    ImportLegacyZiyoumao,
    /// Diagnostics: bootstrap + host checks without a token.
    ProbeProviderHosts,
    Connect,
    Disconnect,
    SelectNode {
        group: String,
        node: String,
    },
    TestGroupDelay(String),
    RefreshGroups,
    SetSystemProxy(bool),
    SetTun(bool),
    ManageService(bool),
    CheckConnectivity,
    DismissNotice,
    /// Error bar retry button: re-sends the failed operation (§17.3).
    RetryNotice,
    SetRoutingMode(RoutingMode),
    SaveSettings(settings::SettingsInput),
    /// Interval (hours, 0 = manual) and egress index for one profile.
    SetUpdatePolicy {
        id: String,
        interval_hours: u32,
        egress: usize,
    },
    /// Copies a URL subscription's link to the clipboard on user request.
    CopyProfileUrl(String),
    UpdateGeodata,
    /// Main window shown/hidden; live streams only run while visible (§17.4).
    WindowVisible(bool),
    CloseHintShown,
    /// Writes the diagnostics bundle to a user-chosen file (§18).
    ExportDiagnostics,
    /// Installs the receiver of traffic samples and log batches.
    SetLiveSink(std::sync::Arc<dyn live::LiveSink>),
    /// Logoff/shutdown: clean up now and acknowledge when done (§11.5).
    SessionEnding(std::sync::mpsc::Sender<()>),
    Quit,
}

enum Internal {
    ServiceManaged {
        generation: u64,
        install: bool,
        enable: bool,
        result: Result<(), AppError>,
    },
    CoreVerified(Result<CoreBinary, AppError>),
    Imported(Result<pipeline::ImportOutcome, AppError>),
    Started(u64, Result<Running, AppError>),
    CoreExited {
        generation: u64,
        detail: String,
    },
    Groups(u64, Result<Vec<ProxyGroupView>, AppError>),
    Stopped(u64),
    DelayDone(u64, String, Result<(), AppError>),
    RestartDue(u64),
    Connectivity(u64, Result<u32, AppError>),
    RemoteAdded(Box<updates::AddedRemote>),
    /// `bool`: started by the user (notices) rather than the schedule.
    Updated(Box<pipeline::UpdateOutcome>, bool),
    Reloaded {
        generation: u64,
        profile_id: ProfileId,
        revision: RevisionId,
        outcome: runtime::ReloadOutcome,
    },
    LoggedIn(Box<Result<crate::providers::ziyoumao::LoggedIn, AppError>>),
    ProviderProfileAdded {
        account_id: AccountId,
        result: Box<Result<pipeline::RemoteAdded, AppError>>,
    },
    AccountRefreshed {
        account_id: AccountId,
        result: Result<crate::providers::ziyoumao::Refreshed, AppError>,
        /// Started by the user (notices) rather than a background check.
        manual: bool,
    },
    HostsProbed(Result<crate::providers::ziyoumao::bootstrap::Discovery, AppError>),
    ModeApplied(u64, Result<(), AppError>),
    CoreMode(u64, Result<String, AppError>),
    GeoUpdated(Result<(), AppError>),
    /// Diagnostics bundle written (or the user cancelled the save dialog).
    DiagnosticsExported(Result<Option<diagnostics::Bundle>, AppError>),
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
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
    let (state_tx, state_rx) = watch::channel(Arc::new(AppStateView::default()));
    let thread = std::thread::Builder::new()
        .name("mirage-core".into())
        .spawn(move || {
            let mut core = AppCore::new(layout, state_tx);
            if let Some(text) = startup_notice {
                core.view.notice = Some(Notice::error(text).with_stage("启动恢复"));
            }
            runtime.block_on(core.run(rx));
            runtime.shutdown_timeout(Duration::from_secs(2));
        })?;
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
    /// Candidate that arrived while a hot reload was running (§6.5).
    queued_reload: Option<(ProfileId, RevisionId)>,
    accounts: accounts::Accounts,
    /// Status before a running session check, restored on inconclusive errors.
    validating_prev: HashMap<AccountId, AccountStatus>,
    live: live::LiveStreams,
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
        let settings: SettingsFile = meta::load(&layout.settings_file(), "app.settings")
            .unwrap_or_else(|e| {
                view.notice = Some(error_notice(&e));
                SettingsFile::default()
            });
        view.system_proxy_wanted = settings.system_proxy;
        view.service_installed = crate::platform::windows::service::installed();
        view.routing_mode = settings.routing_mode;
        let accounts = accounts::Accounts::load(&layout).unwrap_or_else(|e| {
            view.notice = Some(error_notice(&e));
            accounts::Accounts::empty()
        });
        let secrets = SecretStore::new(layout.secrets_dir());
        Self {
            layout,
            state_tx,
            view,
            binary: None,
            profiles,
            running: None,
            generation: 0,
            settings,
            restart_attempts: 0,
            proxy_before_tun: None,
            deferred: Vec::new(),
            secrets,
            updating: HashSet::new(),
            fetch_permits: updates::fetch_permits(),
            queued_reload: None,
            accounts,
            validating_prev: HashMap::new(),
            live: live::LiveStreams::default(),
            internal_tx,
            internal_rx: Some(internal_rx),
        }
    }

    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut internal = self.internal_rx.take().expect("internal receiver");
        self.startup();
        let mut watch = tokio::time::interval(SYSPROXY_WATCH);
        watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first tick fires immediately; it runs before the binary is
        // verified and is a no-op, so the first real check is one tick later.
        let mut schedule = tokio::time::interval(updates::SCHEDULE_TICK);
        schedule.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ack = None;
        loop {
            tokio::select! {
                cmd = commands.recv() => match cmd {
                    Some(Command::Quit) | None => break,
                    Some(Command::SessionEnding(tx)) => {
                        ack = Some(tx);
                        break;
                    }
                    Some(cmd) => self.handle(cmd),
                },
                Some(msg) = internal.recv() => self.handle_internal(msg),
                _ = watch.tick() => self.watch_sysproxy(),
                _ = schedule.tick() => self.schedule_due_updates(),
            }
            self.sync_live_streams();
            self.publish();
        }
        self.shutdown_cleanup().await;
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
        tracing::debug!("app core stopped");
    }

    /// Quit and logoff path: system proxy first, then the core (§11.4, §11.5).
    async fn shutdown_cleanup(&mut self) {
        self.generation += 1;
        self.live.set_window_hidden_for_quit();
        self.release_sysproxy();
        if let Some(running) = self.running.take() {
            runtime::stop(running, &self.layout).await;
        }
    }

    fn startup(&mut self) {
        if let Err(e) = binary::seed_core_home(&self.layout) {
            self.view.notice = Some(error_notice(&e));
        }
        let layout = self.layout.clone();
        self.spawn(async move { Internal::CoreVerified(binary::verify(&layout).await) });
        // §11.6: the saved preference is the source of truth, so a Run entry
        // deleted outside Mirage (or left by an older executable path) is
        // repaired here rather than only when the user opens the settings page.
        if self.settings.autostart
            && let Err(e) = crate::platform::windows::autostart::apply(true)
        {
            tracing::warn!(error = %e, "autostart entry not repaired");
        }
        self.refresh_settings_view();
        self.refresh_profiles_view();
        self.refresh_accounts_view();
        // §16.1: sessions are checked in the background, never blocking connect.
        self.validate_accounts_on_start();
        self.publish();
    }

    fn spawn(&self, fut: impl Future<Output = Internal> + Send + 'static) {
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            // The actor only drops the receiver while shutting down.
            let _ = tx.send(fut.await);
        });
    }

    /// Publishes only when something changed: the UI bridge redraws on
    /// every snapshot, so idle ticks (sysproxy watch, schedule) cost nothing.
    fn publish(&mut self) {
        if let Some(n) = self.view.notice.take() {
            if n.error {
                self.view.error = Some(n);
            } else {
                self.view.toast = Some(n);
            }
        }
        self.state_tx.send_if_modified(|current| {
            if **current == self.view {
                return false;
            }
            *current = Arc::new(self.view.clone());
            true
        });
    }
}

mod handlers;

fn error_notice(e: &AppError) -> Notice {
    Notice::from_error(e)
}

fn info_notice(text: impl Into<String>) -> Notice {
    Notice::info(text)
}

pub fn status_text(status: &RuntimeStatus) -> String {
    match status {
        RuntimeStatus::Stopped => "未连接".into(),
        RuntimeStatus::Starting => "正在连接…".into(),
        RuntimeStatus::Running => "已连接".into(),
        RuntimeStatus::Reloading => "已连接 · 正在应用新配置…".into(),
        RuntimeStatus::Restarting => "内核异常退出，正在重启…".into(),
        RuntimeStatus::Stopping => "正在断开…".into(),
        RuntimeStatus::Failed(reason) => format!("连接失败：{reason}"),
    }
}
