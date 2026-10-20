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
        let name = self.profile_log_name(&running.profile_id);
        self.record_operation(
            "内核异常退出",
            &name,
            "系统",
            "失败",
            "连接中断，准备自动恢复",
        );
        tracing::warn!(%detail, attempts = self.restart_attempts, "core exited unexpectedly");
        runtime::forget_exited(&self.layout);
        self.update_tun_state(false);
        self.view.groups.clear();
        self.view.testing_group = None;
        self.reset_connectivity();
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
        self.connection_source = "自动恢复";
        self.start_after(None);
        if self.view.runtime == RuntimeStatus::Starting {
            self.view.runtime = RuntimeStatus::Restarting;
        } else {
            // Nothing could be started (profile removed, binary missing).
            self.give_up("无法重新启动内核".into());
        }
    }

    fn give_up(&mut self, detail: String) {
        self.connected_since = None;
        self.view.connected_seconds = 0;
        self.update_tun_state(false);
        self.restart_attempts = 0;
        self.release_sysproxy();
        let e = AppError::new(AppErrorKind::CoreExited, STAGE, detail);
        tracing::error!(error = %e, "core restart limit reached");
        // Short status; the exit code and log tail stay in the log.
        self.record_operation(
            "自动恢复连接",
            "",
            "系统",
            "失败",
            "多次重启失败，已停止重试并恢复系统网络设置",
        );
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
                self.record_operation("系统代理", "", "系统", "失败", &e.user_message());
                self.view.notice = Some(error_notice(&e));
                self.view.system_proxy_active = false;
                return;
            }
        }
        if !self.view.system_proxy_active {
            self.record_operation("系统代理", "", "系统", "成功", "已开启");
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
                    self.record_operation("系统代理", "", "系统", "成功", "已关闭并恢复原设置");
                    tracing::info!("system proxy restored");
                }
            }
            Err(e) => {
                self.record_operation("恢复系统代理", "", "系统", "失败", &e.user_message());
                self.view.notice = Some(error_notice(&e));
            }
        }
        self.view.system_proxy_active = false;
    }

    pub(super) fn set_system_proxy(&mut self, on: bool) {
        let changed = self.settings.system_proxy != on;
        self.settings.system_proxy = on;
        if let Err(e) = meta::save(&self.layout.settings_file(), &self.settings, STAGE) {
            self.record_operation("修改系统代理设置", "", "手动", "失败", &e.user_message());
            self.view.notice = Some(error_notice(&e));
            return;
        }
        if changed {
            self.record_operation(
                "修改系统代理设置",
                "",
                "手动",
                "成功",
                if on {
                    "已启用，连接后接管系统代理"
                } else {
                    "已停用"
                },
            );
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
                self.record_operation("系统代理", "", "系统", "成功", "检测到外部修改，已停止接管");
                self.view.system_proxy_active = false;
                self.view.system_proxy_wanted = false;
                self.view.notice = Some(
                    info_notice(Restore::KeptExternal.notice().unwrap_or_default()).notifying(),
                );
            }
            Err(e) => tracing::debug!(error = %e, "system proxy watch failed"),
        }
    }

    fn cancel_connectivity(&mut self) {
        self.connectivity_request += 1;
        if let Some(task) = self.connectivity_task.take() {
            task.abort();
        }
    }

    pub(super) fn reset_connectivity(&mut self) {
        self.cancel_connectivity();
        self.view.connectivity = Connectivity::Unknown;
    }

    pub(super) fn check_connectivity(&mut self) {
        if self.view.runtime != RuntimeStatus::Running {
            return;
        }
        let Some(running) = &self.running else { return };
        let port = running.mixed_port;
        let settling =
            self.view.tun_active && running.started_at.elapsed() < connectivity::SETTLE_WINDOW;
        self.cancel_connectivity();
        let generation = self.generation;
        let request = self.connectivity_request;
        let tx = self.internal_tx.clone();
        self.view.connectivity = Connectivity::Checking;
        self.view_dirty = true;
        self.connectivity_task = Some(tokio::spawn(async move {
            let result = connectivity::probe(port, settling).await;
            let _ = tx.send(Internal::Connectivity(generation, request, result));
        }));
    }

    pub(super) fn on_connectivity(
        &mut self,
        generation: u64,
        request: u64,
        result: Result<u32, AppError>,
    ) {
        if generation != self.generation || request != self.connectivity_request {
            return;
        }
        self.connectivity_task = None;
        self.view.connectivity = match result {
            Ok(ms) => Connectivity::Reachable(ms),
            Err(e) => {
                tracing::info!(error = %e, "connectivity check failed");
                Connectivity::Unreachable(e.user_message())
            }
        };
        self.view_dirty = true;
    }
}
