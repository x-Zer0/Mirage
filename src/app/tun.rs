//! TUN toggling is a restart transaction; installation is an explicit UAC step.
use super::{AppCore, Internal, error_notice, info_notice, state::RuntimeStatus};
use crate::{
    domain::error::{AppError, AppErrorKind},
    platform::windows::service,
};

impl AppCore {
    /// Auto-disable is an entry action; a later manual proxy toggle still wins.
    pub(super) fn update_tun_state(&mut self, active: bool) {
        if active && self.proxy_before_tun.is_none() {
            self.proxy_before_tun = Some(self.view.system_proxy_wanted);
            if self.settings.tun_disable_system_proxy {
                self.view.system_proxy_wanted = false;
                self.release_sysproxy();
            }
        } else if !active && let Some(previous) = self.proxy_before_tun.take() {
            self.view.system_proxy_wanted = previous;
        }
        self.view.tun_active = active;
    }

    pub(super) fn set_tun(&mut self, on: bool) {
        if self.view.service_busy
            || matches!(
                self.view.runtime,
                RuntimeStatus::Starting
                    | RuntimeStatus::Stopping
                    | RuntimeStatus::Reloading
                    | RuntimeStatus::Restarting
            )
        {
            return;
        }
        if on == self.view.tun_wanted {
            return;
        }
        if on && !service::installed() {
            self.manage_service(true, true);
            return;
        }
        self.view.tun_wanted = on;
        if let Some(previous) = self.running.take() {
            self.release_sysproxy();
            self.view.tun_active = false;
            self.view.groups.clear();
            self.start_after(Some(previous));
        } else {
            self.view.notice = Some(info_notice(if on {
                "连接后开启 TUN"
            } else {
                "TUN 已关闭"
            }));
        }
    }

    pub(super) fn manage_service(&mut self, install: bool, enable: bool) {
        if self.view.service_busy
            || self.view.tun_active
            || matches!(
                self.view.runtime,
                RuntimeStatus::Starting
                    | RuntimeStatus::Stopping
                    | RuntimeStatus::Reloading
                    | RuntimeStatus::Restarting
            )
        {
            return;
        }
        self.view.service_busy = true;
        let generation = self.generation;
        let source = self.layout.bundled_core.clone();
        self.spawn(async move {
            let action = if install {
                "安装 / 更新 TUN 辅助服务"
            } else {
                "卸载 TUN 辅助服务"
            };
            let answer = rfd::AsyncMessageDialog::new()
                .set_title(action)
                .set_description("此操作需要 Windows 管理员授权，主界面继续以普通用户运行。")
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show()
                .await;
            let result = if matches!(answer, rfd::MessageDialogResult::Ok) {
                tokio::task::spawn_blocking(move || service::manage(install, &source))
                    .await
                    .map_err(|e| AppError::internal("app.tun", e))
                    .and_then(|r| {
                        r.map_err(|e| {
                            AppError::new(AppErrorKind::Internal, "app.tun", e.to_string())
                        })
                    })
            } else {
                Err(AppError::new(AppErrorKind::Cancelled, "app.tun", ""))
            };
            Internal::ServiceManaged {
                generation,
                install,
                enable,
                result,
            }
        });
    }

    pub(super) fn on_service_managed(
        &mut self,
        generation: u64,
        install: bool,
        enable: bool,
        result: Result<(), AppError>,
    ) {
        self.view.service_busy = false;
        self.view.service_installed = service::installed();
        match result {
            Ok(()) => {
                if !install {
                    self.view.tun_wanted = false;
                }
                self.view.notice = Some(info_notice(if install {
                    "TUN 辅助服务已安装 / 更新"
                } else {
                    "TUN 辅助服务已卸载"
                }));
                if enable && generation == self.generation {
                    self.set_tun(true);
                }
            }
            Err(e) if e.kind == AppErrorKind::Cancelled => {}
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
    }
}
