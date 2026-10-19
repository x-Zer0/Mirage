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
    dirty: bool,
    last_flush: i64,
}

impl OperationLogs {
    pub fn load(layout: &Layout, keep_days: usize) -> AppResult<Self> {
        let dir = layout.operation_logs_dir();
        std::fs::create_dir_all(&dir).map_err(|e| AppError::storage(STAGE, e))?;
        let mut entries = Vec::new();
        for file in std::fs::read_dir(&dir).map_err(|e| AppError::storage(STAGE, e))? {
            let path = file.map_err(|e| AppError::storage(STAGE, e))?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            // M12: 逐文件容错，损坏文件重命名为 .corrupt
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read operation log file");
                    continue;
                }
            };
            let day: Vec<OperationLog> = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "corrupted operation log file");
                    let corrupt_path = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(&path, &corrupt_path);
                    continue;
                }
            };
            entries.extend(day);
        }
        entries.sort_by_key(|e| e.at);
        let mut logs = Self {
            entries,
            dirty: false,
            last_flush: now(),
        };
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
        if let Some(existing) = self.entries.iter_mut().find(|e| e.id == entry.id) {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
        self.dirty = true;
        // M11: 防抖刷盘 - 1 秒内的多次操作只刷一次
        let current = now();
        if current - self.last_flush >= 1 {
            self.flush(layout)?;
        }
        Ok(())
    }

    pub fn flush(&mut self, layout: &Layout) -> AppResult<()> {
        if !self.dirty {
            return Ok(());
        }
        // 按日期分组
        let mut days: std::collections::BTreeMap<PathBuf, Vec<_>> =
            std::collections::BTreeMap::new();
        for entry in &self.entries {
            days.entry(day_path(layout, entry.at))
                .or_default()
                .push(entry);
        }
        for (path, entries) in days {
            let bytes = serde_json::to_vec(&entries).map_err(|e| AppError::storage(STAGE, e))?;
            atomic::write(&path, &bytes).map_err(|e| AppError::storage(STAGE, e))?;
        }
        self.dirty = false;
        self.last_flush = now();
        Ok(())
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
            let path = day_path(layout, at);
            if path.is_file() {
                std::fs::remove_file(path).map_err(|e| AppError::storage(STAGE, e))?;
            }
        }
        self.dirty = true;
        self.flush(layout)
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
        self.dirty = false;
        Ok(())
    }
}

// M13: 修复 expect panic
fn day_path(layout: &Layout, at: i64) -> PathBuf {
    let Some(date) = chrono::DateTime::from_timestamp(at, 0) else {
        // 损坏时间戳，使用 epoch
        tracing::warn!(timestamp = at, "invalid operation timestamp, using epoch");
        return layout.operation_logs_dir().join("1970-01-01.json");
    };
    layout
        .operation_logs_dir()
        .join(format!("{}.json", date.format("%Y-%m-%d")))
}
