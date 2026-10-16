//! Core supervision and system proxy coupling (DESIGN.md §5.2, §11.3, §11.4).
//!
//! An unexpected exit keeps the system proxy and restarts with 1 s / 3 s / 10 s
//! back-off. When the attempts run out the system proxy is withdrawn so the
//! machine is not left pointing at a dead port.

use super::state::{Connectivity, Retry, RuntimeStatus};
use super::sysproxy::{self, Restore};
use super::{AppCore, Internal, connectivity, error_notice, info_notice, runtime};
use crate::domain::error::{AppError, AppErrorKind};
use crate::storage::meta;
use std::time::Duration;

const STAGE: &str = "app.supervise";
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(10),
];
/// A core that ran this long counts as stable; its exit starts a fresh budget.
const STABLE_RUN: Duration = Duration::from_secs(60);

impl AppCore {
    pub(super) fn watch_exit(&self, running: &runtime::Running) {
        let mut exit = running.process.exit_watch();
        let generation = running.generation;
        let log = running.log_file.clone();
        self.spawn(async move {
            let _ = exit.wait_for(Option::is_some).await;
            let code = exit.borrow().and_then(|e| e.code);
            let tail = crate::cores::mihomo::process::log_tail(&log, 3);
            Internal::CoreExited {
                generation,
                detail: format!("退出码 {code:?}；{tail}"),
            }
        });
    }

    pub(super) fn on_core_exited(&mut self, generation: u64, detail: String) {
        if generation != self.generation {
            return; // expected exit after a stop or a superseded start
        }
        let Some(running) = self.running.take() else {
            return;
        };
        if running.started_at.elapsed() >= STABLE_RUN {
            self.restart_attempts = 0;
        }
        tracing::warn!(%detail, attempts = self.restart_attempts, "core exited unexpectedly");
        runtime::forget_exited(&self.layout);
        self.view.groups.clear();
        self.view.testing_group = None;
        self.view.connectivity = Connectivity::Unknown;
        self.schedule_restart(detail);
    }

    /// Next supervised attempt, or give up and withdraw the system proxy.
    pub(super) fn schedule_restart(&mut self, detail: String) {
        let Some(delay) = BACKOFF.get(self.restart_attempts as usize).copied() else {
            self.give_up(detail);
            return;
        };
        self.restart_attempts += 1;
        self.generation += 1;
        let generation = self.generation;
        self.view.runtime = RuntimeStatus::Restarting;
        self.view.notice = Some(info_notice(format!(
            "内核意外退出，{} 秒后第 {}/{} 次重启",
            delay.as_secs(),
            self.restart_attempts,
            BACKOFF.len()
        )));
        self.spawn(async move {
            tokio::time::sleep(delay).await;
            Internal::RestartDue(generation)
        });
    }

    pub(super) fn on_restart_due(&mut self, generation: u64) {
        if generation != self.generation || self.view.runtime != RuntimeStatus::Restarting {
            return; // the user disconnected or reconnected meanwhile
        }
        self.start_after(None);
        if self.view.runtime == RuntimeStatus::Starting {
            self.view.runtime = RuntimeStatus::Restarting;
        } else {
            // Nothing could be started (profile removed, binary missing).
            self.give_up("无法重新启动内核".into());
        }
    }

    fn give_up(&mut self, detail: String) {
        self.restart_attempts = 0;
        self.release_sysproxy();
        let e = AppError::new(AppErrorKind::CoreExited, STAGE, detail);
        tracing::error!(error = %e, "core restart limit reached");
        // Short status; the exit code and log tail stay in the log.
        self.view.runtime = RuntimeStatus::Failed("内核多次意外退出".into());
        self.view.notice = Some(
            error_notice(&AppError::new(
                AppErrorKind::CoreExited,
                STAGE,
                "代理内核已停止，已恢复系统网络设置",
            ))
            .with_retry(Retry::Connect)
            .notifying(),
        );
    }

    /// After the core became ready: point the system proxy at it if wanted
    /// and not already ours (a restart keeps the earlier transaction).
    pub(super) fn ensure_sysproxy(&mut self) {
        if !self.view.system_proxy_wanted {
            return;
        }
        let ours = sysproxy::still_ours(&self.layout).unwrap_or(false);
        if !ours {
            // A stale journal (setting changed meanwhile) must not become `original`.
            sysproxy::forget(&self.layout);
            // The running core's port: a port edited during a start applies next time.
            let port = self
                .running
                .as_ref()
                .map_or(self.view.mixed_port, |r| r.mixed_port);
            if let Err(e) = sysproxy::enable(&self.layout, port, &self.settings.bypass) {
                self.view.notice = Some(error_notice(&e));
                self.view.system_proxy_active = false;
                return;
            }
        }
        self.view.system_proxy_active = true;
    }

    /// Restores the system proxy if we still own it (close flow of §11.3).
    pub(super) fn release_sysproxy(&mut self) {
        match sysproxy::restore(&self.layout) {
            Ok(outcome) => {
                if let Some(text) = outcome.notice() {
                    self.view.notice = Some(info_notice(text));
                }
                if outcome == Restore::Restored {
                    tracing::info!("system proxy restored");
                }
            }
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
        self.view.system_proxy_active = false;
    }

    pub(super) fn set_system_proxy(&mut self, on: bool) {
        self.settings.system_proxy = on;
        if let Err(e) = meta::save(&self.layout.settings_file(), &mut self.settings, STAGE) {
            self.view.notice = Some(error_notice(&e));
        }
        self.view.system_proxy_wanted = on;
        if !on {
            // Also while Restarting: the proxy may still point at the dead port.
            self.release_sysproxy();
        } else if self.view.runtime.is_up() {
            self.ensure_sysproxy();
        }
    }

    /// Detects an external change of an applied system proxy. The change is
    /// respected: the journal is dropped and nothing is re-applied until the
    /// user turns the switch on again or reconnects.
    pub(super) fn watch_sysproxy(&mut self) {
        if !self.view.system_proxy_active {
            return;
        }
        match sysproxy::still_ours(&self.layout) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("system proxy changed by another program");
                sysproxy::forget(&self.layout);
                self.view.system_proxy_active = false;
                self.view.system_proxy_wanted = false;
                self.view.notice = Some(
                    info_notice(Restore::KeptExternal.notice().unwrap_or_default()).notifying(),
                );
            }
            Err(e) => tracing::debug!(error = %e, "system proxy watch failed"),
        }
    }

    pub(super) fn check_connectivity(&mut self) {
        let Some(running) = &self.running else { return };
        if self.view.connectivity == Connectivity::Checking {
            return;
        }
        let port = running.mixed_port;
        let generation = self.generation;
        self.view.connectivity = Connectivity::Checking;
        self.spawn(
            async move { Internal::Connectivity(generation, connectivity::probe(port).await) },
        );
    }

    pub(super) fn on_connectivity(&mut self, generation: u64, result: Result<u32, AppError>) {
        if generation != self.generation {
            return;
        }
        self.view.connectivity = match result {
            Ok(ms) => Connectivity::Reachable(ms),
            Err(e) => {
                tracing::info!(error = %e, "connectivity check failed");
                Connectivity::Unreachable(e.user_message())
            }
        };
    }
}
