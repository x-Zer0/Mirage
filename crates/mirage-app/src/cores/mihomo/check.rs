//! `mihomo -t` configuration check (DESIGN.md §8 step 6, §10.3).

use super::binary::CoreBinary;
use super::process::NO_WINDOW;
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::RevisionId;
use crate::storage::layout::Layout;
use std::time::Duration;

const STAGE: &str = "mihomo.check";
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_LINES: usize = 64;

pub struct CheckReport {
    /// Bounded, redacted-on-display output lines (warnings on success).
    pub lines: Vec<String>,
}

/// Writes `yaml` to `check/<rev>.yaml`, runs the check and removes the file.
pub async fn check(
    core: &CoreBinary,
    layout: &Layout,
    rev: &RevisionId,
    yaml: &str,
) -> AppResult<CheckReport> {
    let file = layout.core_check_dir().join(format!("{rev}.yaml"));
    tokio::fs::write(&file, yaml)
        .await
        .map_err(|e| AppError::storage(STAGE, e))?;
    let result = run(core, layout, &file).await;
    // The check file holds node credentials; removal failure is retried at startup.
    if let Err(e) = tokio::fs::remove_file(&file).await {
        tracing::warn!(error = %e, "check file not removed");
    }
    result
}

async fn run(core: &CoreBinary, layout: &Layout, file: &std::path::Path) -> AppResult<CheckReport> {
    let output = tokio::time::timeout(
        CHECK_TIMEOUT,
        tokio::process::Command::new(&core.exe)
            .arg("-t")
            .arg("-d")
            .arg(&layout.core_home)
            .arg("-f")
            .arg(file)
            .current_dir(&layout.core_home)
            .creation_flags(NO_WINDOW)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| AppError::new(AppErrorKind::Timeout, STAGE, "配置校验超时"))?
    .map_err(|e| AppError::new(AppErrorKind::CoreBinaryInvalid, STAGE, e.to_string()))?;

    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();

    // Always preserve error lines
    let errors: Vec<String> = lines
        .iter()
        .filter(|l| l.contains("level=error") || l.contains("level=fatal"))
        .take(3)
        .cloned()
        .collect();

    // Keep tail window
    if lines.len() > MAX_LINES {
        lines.drain(0..(lines.len() - MAX_LINES));
    }

    // Append errors that aren't already in the window
    for error in errors {
        if !lines.contains(&error) {
            lines.push(error);
        }
    }

    if output.status.success() {
        Ok(CheckReport { lines })
    } else {
        let reason = lines
            .iter()
            .rev()
            .find(|l| l.contains("level=error") || l.contains("level=fatal"))
            .or(lines.last())
            .cloned()
            .unwrap_or_else(|| format!("退出码 {:?}", output.status.code()));
        Err(AppError::new(
            AppErrorKind::ConfigCheckFailed,
            STAGE,
            reason,
        ))
    }
}
