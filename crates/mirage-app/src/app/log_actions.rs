//! Business log recording and bounded, searchable pages. UI state never owns history.
use super::AppCore;
use super::logs::{ConnectionLog, LogQuery, LogsPage, OperationLog};
use crate::domain::model::{new_id, now};
use crate::domain::redact::redact_text;
use std::sync::Arc;

const CONNECTION_FILTERS: [&str; 5] = ["", "代理", "直连", "拦截", "失败"];
const OPERATION_FILTERS: [&str; 4] = ["", "成功", "失败", "进行中"];

impl AppCore {
    pub(super) fn query_logs(&mut self, mut query: LogQuery) {
        query.text = query.text.trim().to_lowercase();
        if !query.operations && query.paused {
            query.anchor = self
                .log_query
                .anchor
                .or_else(|| Some(self.connection_logs.rows.back().map_or(0, |r| r.id)));
        }
        self.log_query = query;
        self.refresh_logs();
    }

    pub(super) fn clear_connection_logs(&mut self) {
        self.suspend_connection_stream();
        self.connection_logs.clear(self.live.epoch);
        // 清空后不再固定 anchor，让新日志正常显示
        self.log_query.anchor = None;
        self.restart_live_streams();
        self.refresh_logs();
    }

    pub(super) fn on_connections(&mut self, epoch: u64, rows: Vec<ConnectionLog>) {
        if epoch < self.connection_logs.min_epoch {
            return;
        }
        for row in rows {
            self.connection_logs
                .push(row, self.settings.connection_log_limit);
        }
        self.refresh_logs();
    }

    pub(super) fn set_log_status(&mut self, status: String) {
        self.live.status = status;
        self.refresh_logs();
    }

    pub(super) fn refresh_logs(&mut self) {
        if !self.log_query.active || !self.live.visible {
            return;
        }
        let q = &self.log_query;
        let mut page = LogsPage {
            revision: self.view.logs.revision + 1,
            status: self.live.status.clone(),
            ..Default::default()
        };
        if q.operations {
            page.operations = Arc::new(
                self.operation_logs
                    .entries()
                    .iter()
                    .rev()
                    .filter(|r| {
                        (q.filter == 0 || r.result == operation_filter(q.filter))
                            && matches_text(
                                &q.text,
                                &[&r.action, &r.object, &r.source, &r.result, &r.summary],
                            )
                    })
                    .cloned()
                    .collect(),
            );
            page.total = page.operations.len();
        } else {
            page.connections = Arc::new(
                self.connection_logs
                    .rows
                    .iter()
                    .rev()
                    .filter(|r| {
                        (q.filter == 0 || r.action == connection_filter(q.filter))
                            && matches_text(
                                &q.text,
                                &[&r.address, &r.rule, &r.profile, &r.node, &r.details],
                            )
                            && q.anchor.is_none_or(|id| r.id <= id)
                    })
                    .cloned()
                    .collect(),
            );
            page.total = page.connections.len();
        }
        self.view.logs = Arc::new(page);
        self.view_dirty = true;
    }

    pub(super) fn record_operation(
        &mut self,
        action: &str,
        object: &str,
        source: &str,
        result: &str,
        summary: &str,
    ) -> String {
        let entry = OperationLog {
            id: new_id(),
            at: now(),
            action: action.into(),
            object: redact_text(object).into_owned(),
            source: source.into(),
            result: result.into(),
            summary: redact_text(summary).into_owned(),
        };
        let id = entry.id.clone();
        if self.settings.operation_log_days > 0 {
            self.store_operation(entry);
        }
        id
    }

    /// Helper: record operation and set error/toast notice from result.
    #[allow(dead_code)]
    pub(super) fn report_result<T>(
        &mut self,
        action: &str,
        object: &str,
        source: &str,
        result: &Result<T, crate::domain::error::AppError>,
    ) {
        match result {
            Ok(_) => {
                self.record_operation(action, object, source, "成功", "");
            }
            Err(e) => {
                self.record_operation(action, object, source, "失败", &e.user_message());
                self.view.notice = Some(super::error_notice(e));
            }
        }
    }

    fn store_operation(&mut self, entry: OperationLog) {
        if let Err(e) = self.operation_logs.store(&self.layout, entry) {
            self.view.notice = Some(super::error_notice(&e));
        }
        self.refresh_logs();
    }

    pub(super) fn begin_update_log(&mut self, profile_id: &str, manual: bool) {
        if self.update_operations.contains_key(profile_id) {
            return;
        }
        let name = self.profile_log_name(profile_id);
        let id = self.record_operation(
            "更新订阅",
            &name,
            if manual {
                "手动更新"
            } else {
                "自动更新"
            },
            "进行中",
            "正在下载并校验",
        );
        self.update_operations.insert(profile_id.into(), id);
    }

    pub(super) fn finish_update_log(&mut self, profile_id: &str, result: &str, summary: &str) {
        let Some(id) = self.update_operations.remove(profile_id) else {
            return;
        };
        let Some(mut entry) = self
            .operation_logs
            .entries()
            .iter()
            .find(|e| e.id == id)
            .cloned()
        else {
            return;
        };
        entry.result = result.into();
        entry.summary = redact_text(summary).into_owned();
        if self.settings.operation_log_days > 0 {
            self.store_operation(entry);
        }
    }

    pub(super) fn profile_log_name(&self, id: &str) -> String {
        self.profiles
            .get(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "未提供".into())
    }

    pub(super) fn interrupt_update_logs(&mut self) {
        let ids: Vec<_> = self.update_operations.keys().cloned().collect();
        for id in ids {
            self.finish_update_log(&id, "失败", "应用退出，未取得最终结果");
        }
    }

    pub(super) fn prune_operation_logs(&mut self) {
        if self.settings.operation_log_days == 0 {
            let _ = self.operation_logs.clear(&self.layout);
        } else if let Err(e) = self
            .operation_logs
            .prune(&self.layout, self.settings.operation_log_days)
        {
            tracing::warn!(error = %e, "operation history cleanup failed");
        }
        self.refresh_logs();
    }
}

fn matches_text(query: &str, values: &[&str]) -> bool {
    query.is_empty() || values.iter().any(|v| v.to_lowercase().contains(query))
}
fn connection_filter(filter: usize) -> &'static str {
    CONNECTION_FILTERS.get(filter).copied().unwrap_or("")
}
fn operation_filter(filter: usize) -> &'static str {
    OPERATION_FILTERS.get(filter).copied().unwrap_or("")
}
