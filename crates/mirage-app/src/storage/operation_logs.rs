//! Daily operation history. This is a new schema without legacy readers.
use crate::app::logs::OperationLog;
use crate::domain::error::{AppError, AppResult};
use crate::domain::model::now;
use crate::storage::{atomic, layout::Layout};
use std::path::PathBuf;

const STAGE: &str = "storage.operation-logs";

#[derive(Debug, Default)]
pub struct OperationLogs {
    entries: Vec<OperationLog>,
}

impl OperationLogs {
    pub fn load(layout: &Layout, keep_days: usize) -> AppResult<Self> {
        let dir = layout.operation_logs_dir();
        std::fs::create_dir_all(&dir).map_err(|e| AppError::storage(STAGE, e))?;
        let mut entries = Vec::new();
        for file in std::fs::read_dir(dir).map_err(|e| AppError::storage(STAGE, e))? {
            let path = file.map_err(|e| AppError::storage(STAGE, e))?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let bytes = std::fs::read(path).map_err(|e| AppError::storage(STAGE, e))?;
            let day: Vec<OperationLog> =
                serde_json::from_slice(&bytes).map_err(|e| AppError::storage(STAGE, e))?;
            entries.extend(day);
        }
        entries.sort_by_key(|e| e.at);
        let mut logs = Self { entries };
        logs.prune(layout, keep_days)?;
        let interrupted: Vec<_> = logs
            .entries
            .iter()
            .filter(|e| e.result == "进行中")
            .cloned()
            .collect();
        for mut entry in interrupted {
            entry.result = "失败".into();
            entry.summary = "上次运行期间操作中断，未取得最终结果".into();
            logs.store(layout, entry)?;
        }
        Ok(logs)
    }

    pub fn entries(&self) -> &[OperationLog] {
        &self.entries
    }

    pub fn store(&mut self, layout: &Layout, entry: OperationLog) -> AppResult<()> {
        let at = entry.at;
        if let Some(existing) = self.entries.iter_mut().find(|e| e.id == entry.id) {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
        self.save_day(layout, at)
    }

    pub fn prune(&mut self, layout: &Layout, keep_days: usize) -> AppResult<()> {
        if keep_days == 0 {
            return self.clear(layout);
        }
        let cutoff = now() - (keep_days as i64) * 24 * 60 * 60;
        let expired_days: std::collections::BTreeSet<_> = self
            .entries
            .iter()
            .filter(|e| e.at < cutoff)
            .map(|e| e.at.div_euclid(86400) * 86400)
            .collect();
        self.entries.retain(|e| e.at >= cutoff);
        for at in expired_days {
            self.save_day(layout, at)?;
        }
        Ok(())
    }
    pub fn clear(&mut self, layout: &Layout) -> AppResult<()> {
        self.entries.clear();
        let dir = layout.operation_logs_dir();
        for file in std::fs::read_dir(&dir).map_err(|e| AppError::storage(STAGE, e))? {
            let path = file.map_err(|e| AppError::storage(STAGE, e))?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                std::fs::remove_file(path).map_err(|e| AppError::storage(STAGE, e))?;
            }
        }
        Ok(())
    }

    fn save_day(&self, layout: &Layout, at: i64) -> AppResult<()> {
        let path = day_path(layout, at);
        let day: Vec<_> = self
            .entries
            .iter()
            .filter(|e| day_path(layout, e.at) == path)
            .collect();
        if day.is_empty() {
            if path.is_file() {
                std::fs::remove_file(path).map_err(|e| AppError::storage(STAGE, e))?;
            }
            return Ok(());
        }
        let bytes = serde_json::to_vec(&day).map_err(|e| AppError::storage(STAGE, e))?;
        atomic::write(&path, &bytes).map_err(|e| AppError::storage(STAGE, e))
    }
}

fn day_path(layout: &Layout, at: i64) -> PathBuf {
    let date = chrono::DateTime::from_timestamp(at, 0).expect("operation timestamp");
    layout
        .operation_logs_dir()
        .join(format!("{}.json", date.format("%Y-%m-%d")))
}
