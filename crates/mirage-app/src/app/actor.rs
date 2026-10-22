//! Actor initialization, event loop and startup/shutdown lifecycle.
use super::state::AppStateView;
use super::{
    AppCore, Command, DEFAULT_MIXED_PORT, Internal, accounts, error_notice, live, profiles,
    runtime, updates,
};
use crate::cores::mihomo::binary;
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::platform::windows::secret::SecretStore;
use crate::storage::{layout::Layout, meta::SettingsFile, operation_logs::OperationLogs};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, watch};

const SYSPROXY_WATCH: Duration = Duration::from_secs(5);
const CONNECTION_CLOCK: Duration = Duration::from_secs(1);
const STAGE: &str = "app.handlers";

impl AppCore {
    pub(super) fn new(
        layout: Layout,
        state_tx: watch::Sender<Arc<AppStateView>>,
        settings: AppResult<SettingsFile>,
    ) -> Self {
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
        let settings = settings.unwrap_or_else(|e| {
            view.notice = Some(error_notice(&e));
            SettingsFile::default()
        });
        view.system_proxy_wanted = settings.system_proxy;
        view.tun_wanted = settings.tun_enabled;
        view.service_installed = crate::platform::windows::service::installed();
        view.routing_mode = settings.routing_mode;
        let operation_logs = OperationLogs::load(&layout, settings.operation_log_days)
            .unwrap_or_else(|e| {
                view.notice = Some(error_notice(&e));
                OperationLogs::default()
            });
        let accounts = accounts::Accounts::load(&layout).unwrap_or_else(|e| {
            view.notice = Some(error_notice(&e));
            accounts::Accounts::empty()
        });
        let secrets = SecretStore::new(layout.secrets_dir());
        let hosts = crate::overrides::hosts::Hosts::load(&layout).unwrap_or_else(|e| {
            view.notice = Some(error_notice(&e));
            Default::default()
        });
        let rules = crate::overrides::rules::Rules::load(&layout).unwrap_or_else(|e| {
            view.notice = Some(error_notice(&e));
            Default::default()
        });
        Self {
            layout,
            state_tx,
            view,
            view_dirty: true,
            binary: None,
            profiles,
            running: None,
            generation: 0,
            delay_tests: Default::default(),
            connectivity_request: 0,
            connectivity_task: None,
            settings,
            ui_geometry_save_at: None,
            restart_attempts: 0,
            connection_source: "系统",
            connected_since: None,
            deferred: Vec::new(),
            secrets,
            updating: HashSet::new(),
            fetch_permits: updates::fetch_permits(),
            queued_reload: None,
            accounts,
            validating_prev: HashMap::new(),
            live: live::LiveStreams::default(),
            operation_logs,
            connection_logs: Default::default(),
            log_query: Default::default(),
            update_operations: HashMap::new(),
            hosts,
            rules,
            failed_overlay: None,
            internal_tx,
            internal_rx: Some(internal_rx),
        }
    }

    pub(super) async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut internal = self.internal_rx.take().expect("internal receiver");
        self.startup();
        let mut watch = tokio::time::interval(SYSPROXY_WATCH);
        watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first tick fires immediately; it runs before the binary is
        // verified and is a no-op, so the first real check is one tick later.
        let mut schedule = tokio::time::interval(updates::SCHEDULE_TICK);
        schedule.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut connection_clock = tokio::time::interval(CONNECTION_CLOCK);
        connection_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ack = None;
        loop {
            let geometry_save_at = self.ui_geometry_save_at;
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
                _ = tokio::time::sleep_until(geometry_save_at.unwrap_or_else(tokio::time::Instant::now)),
                    if geometry_save_at.is_some() => self.flush_ui_geometry(),
                _ = watch.tick() => self.watch_sysproxy(),
                _ = connection_clock.tick() => {
                    self.update_connection_clock();
                    self.expire_node_delays();
                },
                _ = schedule.tick() => { self.schedule_due_updates(); self.prune_operation_logs(); },
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
        self.flush_ui_geometry();
        self.reset_delay_tests();
        self.generation += 1;
        self.live.set_window_hidden_for_quit();
        self.settings.resume_connected = self.view.runtime.is_up()
            || matches!(
                self.view.runtime,
                super::state::RuntimeStatus::Starting | super::state::RuntimeStatus::Restarting
            );
        self.settings.tun_enabled = self.view.tun_wanted;
        if let Err(e) = self.save_settings_file() {
            tracing::warn!(error = %e, "save session state failed");
        }
        self.release_sysproxy();
        self.interrupt_update_logs();
        if let Some(running) = self.running.take() {
            runtime::stop(running, &self.layout).await;
            self.record_operation(
                "断开连接",
                "",
                "系统",
                "成功",
                "退出 Mirage，代理内核已停止",
            );
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
        self.refresh_override_views();
        self.refresh_profiles_view();
        self.refresh_accounts_view();
        // §16.1: sessions are checked in the background, never blocking connect.
        self.validate_accounts_on_start();
        self.publish();
    }

    pub(super) fn spawn(&self, fut: impl Future<Output = Internal> + Send + 'static) {
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
            self.view_dirty = true;
            if n.error {
                self.view.error = Some(n);
            } else {
                self.view.toast = Some(n);
            }
        }
        if self.view_dirty {
            self.state_tx.send_if_modified(|current| {
                *current = Arc::new(self.view.clone());
                true
            });
            self.view_dirty = false;
        }
    }

    pub(super) fn core_checked(&self) -> bool {
        self.binary.is_some() || self.view.core_version == "不可用"
    }

    pub(super) fn on_core_verified(
        &mut self,
        result: Result<crate::cores::mihomo::binary::CoreBinary, AppError>,
    ) {
        match result {
            Ok(binary) => {
                tracing::info!(version = %binary.version_line, "core verified");
                self.view.core_version = binary.short_version();
                self.view.core_ready = true;
                self.binary = Some(binary);
            }
            Err(e) => {
                self.view.core_version = "不可用".into();
                self.view.notice = Some(error_notice(&e));
            }
        }
        for cmd in std::mem::take(&mut self.deferred) {
            self.handle(cmd);
        }
        // §16.1: start from the stored revision, never waiting on the network.
        if self.binary.is_some()
            && (self.settings.auto_connect || self.settings.resume_connected)
            && self.profiles.selected().is_some()
        {
            tracing::info!("auto-connect on startup");
            self.connect(false);
        }
        self.migrate_provider_profiles();
    }

    fn update_connection_clock(&mut self) {
        let Some(started) = self.connected_since else {
            return;
        };
        let seconds = started.elapsed().as_secs().min(99 * 3600 + 59 * 60 + 59) as u32;
        if self.view.connected_seconds != seconds {
            self.view.connected_seconds = seconds;
            self.view_dirty = true;
        }
    }

    pub(super) fn require_binary(&mut self) -> Option<crate::cores::mihomo::binary::CoreBinary> {
        if self.binary.is_none() {
            self.view.notice = Some(error_notice(&AppError::new(
                AppErrorKind::CoreBinaryInvalid,
                STAGE,
                "内核尚未通过校验",
            )));
        }
        self.binary.clone()
    }
}
