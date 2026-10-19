//! Connection transactions and core startup/completion handling.
use super::state::{Retry, RuntimeStatus};
use super::{AppCore, Internal, error_notice, info_notice, runtime};
use crate::domain::error::{AppError, AppErrorKind};
use crate::platform::windows::conflict::{self, Conflict};

impl AppCore {
    pub(super) fn connect(&mut self, manual: bool) {
        if self.view.service_busy {
            return;
        }
        if matches!(
            self.view.runtime,
            RuntimeStatus::Starting
                | RuntimeStatus::Running
                | RuntimeStatus::Reloading
                | RuntimeStatus::Stopping
        ) {
            return;
        }
        // §11.8: warn about the original client before starting. This never
        // blocks the connect: the user may be running it deliberately, and the
        // check exists to explain a failure, not to prevent an attempt.
        let conflicts = conflict::detect(self.view.mixed_port);
        if !conflicts.is_empty() {
            tracing::warn!(
                count = conflicts.len(),
                "conflicts with the original client"
            );
            let text = conflicts
                .iter()
                .map(Conflict::text)
                .collect::<Vec<_>>()
                .join("；");
            self.view.notice = Some(
                info_notice(format!("提示：{text}。若连接失败请先完全退出原客户端。")).notifying(),
            );
        }
        // An explicit connect starts a fresh restart budget and re-reads the
        // saved switch (an external change may have turned it off for this run).
        self.restart_attempts = 0;
        self.connection_source = if manual { "手动" } else { "自动连接" };
        self.view.system_proxy_wanted = self.settings.system_proxy;
        self.start_after(None);
    }

    /// Starts the selected profile, first stopping `previous` in the same task.
    pub(super) fn start_after(&mut self, previous: Option<runtime::Running>) {
        let Some(binary) = self.require_binary() else {
            return;
        };
        let Some(profile) = self.profiles.selected().cloned() else {
            self.view.notice = Some(info_notice("请先导入并选择一个配置"));
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.reset_connectivity();
        self.view.runtime = RuntimeStatus::Starting;
        self.view_dirty = true;
        self.view.error = None;
        let req = runtime::StartRequest {
            generation,
            layout: self.layout.clone(),
            binary,
            profile,
            mixed_port: self.view.mixed_port,
            mode: self.settings.routing_mode,
            tun: self.view.tun_wanted,
        };
        self.spawn(async move {
            if let Some(previous) = previous {
                runtime::stop(previous, &req.layout).await;
            }
            Internal::Started(generation, runtime::start(req).await)
        });
    }

    pub(super) fn on_started(
        &mut self,
        generation: u64,
        result: Result<runtime::Running, AppError>,
    ) {
        match result {
            Ok(running) if generation == self.generation => {
                if self.view.runtime == RuntimeStatus::Restarting {
                    tracing::info!(attempt = self.restart_attempts, "core restarted");
                    self.view.notice = Some(info_notice("内核已自动重启"));
                }
                let name = self.profile_log_name(&running.profile_id);
                let action = if self.view.runtime == RuntimeStatus::Restarting {
                    "自动恢复连接"
                } else {
                    "连接"
                };
                self.record_operation(
                    action,
                    &name,
                    self.connection_source,
                    "成功",
                    "配置已启动，代理内核运行中",
                );
                self.view.runtime = RuntimeStatus::Running;
                self.view.check_lines = running.check_lines.clone();
                self.view.policy_notes = running.policy_notes.clone();
                self.update_tun_state(running.process.service().is_some());
                self.watch_exit(&running);
                self.running = Some(running);
                self.ensure_sysproxy();
                self.refresh_groups();
                self.read_core_mode();
                self.check_connectivity();
                // The controller address changed with the new process.
                self.restart_live_streams();
            }
            Ok(stale) => {
                // A newer transaction superseded this start; discard its core.
                let layout = self.layout.clone();
                tokio::spawn(async move { runtime::stop(stale, &layout).await });
            }
            Err(e) if generation == self.generation => {
                let name = self
                    .profiles
                    .selected()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let action = if self.view.runtime == RuntimeStatus::Restarting {
                    "自动恢复连接"
                } else {
                    "连接"
                };
                self.record_operation(
                    action,
                    &name,
                    self.connection_source,
                    "失败",
                    &e.user_message(),
                );
                if self.view.tun_wanted && self.view.runtime != RuntimeStatus::Restarting {
                    self.view.tun_wanted = false;
                    self.update_tun_state(false);
                    self.start_after(None);
                    self.view.notice = Some(error_notice(&e.append("；正在恢复普通代理模式")));
                    return;
                }
                if self.view.runtime == RuntimeStatus::Restarting {
                    self.schedule_restart(e.user_message());
                } else {
                    // A failed user connect may follow a crash; never leave the
                    // system proxy pointing at a port nobody listens on.
                    self.release_sysproxy();
                    // The status names the failure; the error bar carries the
                    // detail (which may include the core's log tail).
                    let reason = if e.kind == AppErrorKind::ServiceUnavailable {
                        e.user_message()
                    } else {
                        e.kind.label().to_owned()
                    };
                    self.view.runtime = RuntimeStatus::Failed(reason);
                    self.view.notice = Some(error_notice(&e).with_retry(Retry::Connect));
                }
            }
            Err(_) => {}
        }
        if generation == self.generation {
            self.commit_queued_revision();
            self.migrate_provider_profiles();
        }
    }

    /// User disconnect: system proxy first, then the core (§11.4).
    pub(super) fn disconnect(&mut self) {
        self.view_dirty = true;
        self.generation += 1;
        self.restart_attempts = 0;
        self.release_sysproxy();
        self.update_tun_state(false);
        self.view.groups.clear();
        self.view.testing_group = None;
        self.reset_connectivity();
        let Some(running) = self.running.take() else {
            self.view.runtime = RuntimeStatus::Stopped;
            return;
        };
        self.view.runtime = RuntimeStatus::Stopping;
        let layout = self.layout.clone();
        let generation = self.generation;
        self.spawn(async move {
            runtime::stop(running, &layout).await;
            Internal::Stopped(generation)
        });
    }
}
