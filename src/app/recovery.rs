//! Startup recovery (DESIGN.md §11.5). Runs in `main` before the UI and the
//! app core exist, so it never depends on a window-close callback having run.

use super::sysproxy::{self, Restore};
use crate::platform::windows::process;
use crate::storage::journal;
use crate::storage::layout::Layout;

/// Undoes what a previous run left behind. Returns a notice for the user, if any.
pub fn run(layout: &Layout) -> Option<String> {
    let notice = restore_sysproxy(layout);
    let tun_notice = recover_tun(layout);
    kill_leftover_core(layout);
    clean_check_dir(layout);
    super::runtime::remove_run_config(layout);
    tun_notice.or(notice)
}

fn restore_sysproxy(layout: &Layout) -> Option<String> {
    match sysproxy::restore(layout) {
        Ok(Restore::Restored) => {
            tracing::warn!("restored system proxy left by an abnormal exit");
            Some("上次未正常退出，已还原系统代理设置".into())
        }
        Ok(Restore::KeptExternal) => Restore::KeptExternal.notice().map(Into::into),
        Ok(Restore::Nothing) => None,
        Err(e) => {
            tracing::error!(error = %e, "system proxy recovery failed");
            Some(e.user_message())
        }
    }
}

/// Ends a core from an earlier run only if PID, creation time and image path
/// all still match the journal (§10.4); a reused PID is left alone.
fn kill_leftover_core(layout: &Layout) {
    let recorded = match journal::load_core(layout) {
        Ok(Some(j)) => j,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "core journal unreadable");
            journal::remove(&journal::core_path(layout));
            return;
        }
    };
    match process::identity(recorded.pid) {
        Some(found) if found.matches(&recorded.identity()) => {
            match process::terminate(recorded.pid) {
                Ok(()) => tracing::warn!(pid = recorded.pid, "terminated leftover core"),
                Err(e) => tracing::error!(pid = recorded.pid, error = %e, "leftover core kept"),
            }
        }
        Some(_) => tracing::info!(
            pid = recorded.pid,
            "recorded PID now belongs to another process"
        ),
        None => {}
    }
    journal::remove(&journal::core_path(layout));
}

/// Removes leftover check files; they may hold node credentials.
pub fn clean_check_dir(layout: &Layout) {
    let Ok(entries) = std::fs::read_dir(layout.core_check_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

fn recover_tun(layout: &Layout) -> Option<String> {
    let path = layout.recovery.join("tun.json");
    if !path.exists() {
        return None;
    }
    if !crate::platform::windows::service::installed() {
        journal::remove(&path);
        return None;
    }
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
        .and_then(|rt| {
            rt.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(15),
                    crate::cores::mihomo::service::cleanup(),
                )
                .await
                .map_err(|_| "TUN 恢复超时".to_owned())?
                .map_err(|e| e.user_message())
            })
        });
    match result {
        Ok(()) => {
            journal::remove(&path);
            Some("已清理上次遗留的 TUN 内核".into())
        }
        Err(e) => Some(format!("TUN 恢复未完成，已保留恢复记录：{e}")),
    }
}
