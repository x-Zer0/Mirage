//! UWP loopback exemption dialog (iteration 2 §6): list as the user, write
//! directly when allowed, otherwise through one elevated child process.
use super::{AppCore, Internal, error_notice, info_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::platform::windows::{loopback, service};

const STAGE: &str = "app.loopback";
/// Windows command lines are limited to 32,767 characters.
const MAX_ARGS_LEN: usize = 32_000;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_CANCELLED: i32 = 1223;

impl AppCore {
    pub(super) fn list_loopback(&mut self) {
        if self.view.loopback.loading || self.view.loopback.saving {
            return;
        }
        self.view.loopback.loading = true;
        self.spawn(async move {
            let result = tokio::task::spawn_blocking(loopback::list)
                .await
                .map_err(|e| AppError::internal(STAGE, e))
                .and_then(|r| {
                    r.map_err(|e| AppError::internal(STAGE, format!("读取应用列表失败：{e}")))
                });
            Internal::LoopbackListed(result)
        });
    }

    pub(super) fn on_loopback_listed(&mut self, result: AppResult<Vec<loopback::AppContainer>>) {
        self.view.loopback.loading = false;
        match result {
            Ok(apps) => {
                self.view.loopback.revision += 1;
                self.view.loopback.apps = std::sync::Arc::new(apps);
            }
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
    }

    /// `states`: SID and wanted state of each app the user changed. Apps
    /// not listed (unchanged, other users, removed packages) keep theirs.
    pub(super) fn save_loopback(&mut self, states: Vec<(String, bool)>) {
        if self.view.loopback.saving {
            return;
        }
        if states.is_empty() {
            self.view.notice = Some(info_notice("没有修改，UWP 回环豁免保持不变"));
            return;
        }
        self.view.loopback.saving = true;
        self.spawn(async move {
            let result = tokio::task::spawn_blocking(move || apply(&states))
                .await
                .map_err(|e| AppError::internal(STAGE, e))
                .and_then(|r| r);
            Internal::LoopbackSaved(result)
        });
    }

    pub(super) fn on_loopback_saved(&mut self, result: AppResult<usize>) {
        self.view.loopback.saving = false;
        match result {
            Ok(count) => {
                self.record_operation(
                    "设置 UWP 回环豁免",
                    "",
                    "手动",
                    "成功",
                    &format!("已豁免 {count} 个应用"),
                );
                self.view.notice = Some(info_notice(format!(
                    "UWP 回环豁免已保存，共 {count} 个应用"
                )));
            }
            Err(e) if e.kind == AppErrorKind::Cancelled => {
                self.view.notice = Some(info_notice("已取消，UWP 回环豁免未修改"));
            }
            Err(e) => {
                self.record_operation("设置 UWP 回环豁免", "", "手动", "失败", &e.user_message());
                self.view.notice = Some(error_notice(&e));
            }
        }
        // Re-read so the dialog shows what Windows actually holds.
        self.list_loopback();
    }
}

/// Current list − unchecked + checked, written as a whole (§6.4).
fn apply(states: &[(String, bool)]) -> AppResult<usize> {
    let io = |e: std::io::Error| AppError::internal(STAGE, e);
    let mut target = loopback::exempt_sids().map_err(io)?;
    target.retain(|sid| !states.iter().any(|(s, on)| s == sid && !on));
    for (sid, on) in states {
        if *on && !target.contains(sid) {
            target.push(sid.clone());
        }
    }
    if target.iter().any(|s| !loopback::valid_sid(s)) {
        return Err(AppError::new(
            AppErrorKind::InvalidInput,
            STAGE,
            "SID 不合法",
        ));
    }
    match loopback::set_exempt(&target) {
        Ok(()) => return Ok(target.len()),
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {}
        Err(e) => return Err(io(e)),
    }
    let args = std::iter::once(crate::SET_LOOPBACK_EXEMPT.to_owned())
        .chain(target.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    if args.len() > MAX_ARGS_LEN {
        return Err(AppError::new(
            AppErrorKind::InvalidInput,
            STAGE,
            "选择的应用过多，请减少后重试",
        ));
    }
    let exe = std::env::current_exe().map_err(io)?;
    match service::run_elevated(&exe, &args) {
        Ok(0) => Ok(target.len()),
        Ok(code) => Err(AppError::internal(
            STAGE,
            format!("写入失败（错误码 {code}）"),
        )),
        Err(e) if e.raw_os_error() == Some(ERROR_CANCELLED) => {
            Err(AppError::new(AppErrorKind::Cancelled, STAGE, ""))
        }
        Err(e) => Err(io(e)),
    }
}
