//! Traffic follows window visibility; routing events are collected for the whole session.
use super::logs::{ConnectionLog, friendly_rule, parse_connection};
use super::state::TrafficSample;
use super::{AppCore, Internal};
use crate::cores::mihomo::controller::Controller;
use serde::Deserialize;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};

const LOG_FLUSH: Duration = Duration::from_millis(500);
const LOG_BATCH: usize = 200;
const RECONNECT: Duration = Duration::from_secs(2);

pub trait LiveSink: Send + Sync + std::fmt::Debug {
    fn traffic(&self, sample: TrafficSample);
}

#[derive(Default)]
pub(super) struct LiveStreams {
    sink: Option<Arc<dyn LiveSink>>,
    pub visible: bool,
    pub status: String,
    pub epoch: u64,
    traffic: Option<JoinHandle<()>>,
    logs: Option<JoinHandle<()>>,
}
impl LiveStreams {
    pub fn set_sink(&mut self, sink: Arc<dyn LiveSink>) {
        self.sink = Some(sink);
    }
    pub fn set_window_hidden_for_quit(&mut self) {
        self.visible = false;
        self.stop();
    }
    fn stop(&mut self) {
        self.epoch += 1;
        if let Some(task) = self.logs.take() {
            task.abort();
        }
        self.stop_traffic();
    }
    fn stop_traffic(&mut self) {
        let Some(task) = self.traffic.take() else {
            return;
        };
        task.abort();
        if let Some(sink) = &self.sink {
            sink.traffic(TrafficSample::default());
        }
    }
}

impl AppCore {
    pub(super) fn set_window_visible(&mut self, visible: bool) {
        self.live.visible = visible;
        self.refresh_logs();
    }

    pub(super) fn sync_live_streams(&mut self) {
        let up = self.view.runtime.is_up() && self.running.is_some();
        if !up {
            self.live.stop();
            if self.live.status != "未连接" {
                self.set_log_status("未连接".into());
            }
            return;
        }
        let running = self.running.as_ref().expect("running core");
        if self.live.visible && self.live.traffic.is_none() {
            if let Some(sink) = self.live.sink.clone() {
                self.live.traffic =
                    Some(tokio::spawn(traffic_loop(running.controller.clone(), sink)));
            }
        } else if !self.live.visible && self.live.traffic.is_some() {
            self.live.stop_traffic();
        }
        if self.live.logs.is_none() && self.view.runtime != super::state::RuntimeStatus::Reloading {
            self.live.epoch += 1;
            let profile = self.profiles.get(&running.profile_id);
            let context = ConnectionContext {
                profile: profile
                    .map(|p| format!("{} · {}", p.source.label(), p.name))
                    .unwrap_or_else(|| "未提供".into()),
                identity: profile
                    .map(|p| {
                        format!(
                            "配置 ID：{}\n版本：{}",
                            p.id,
                            p.active_revision.as_deref().unwrap_or("未提供")
                        )
                    })
                    .unwrap_or_default(),
            };
            self.live.logs = Some(tokio::spawn(connection_loop(
                running.controller.clone(),
                self.internal_tx.clone(),
                self.live.epoch,
                context,
            )));
            self.set_log_status("正在连接日志流…".into());
        }
    }

    pub(super) fn restart_live_streams(&mut self) {
        self.live.stop();
        self.sync_live_streams();
    }
    pub(super) fn suspend_connection_stream(&mut self) {
        self.live.epoch += 1;
        if let Some(task) = self.live.logs.take() {
            task.abort();
        }
        self.set_log_status("配置正在切换，采集暂时中断".into());
    }
}

struct ConnectionContext {
    profile: String,
    identity: String,
}
impl ConnectionContext {
    fn parse(&self, payload: &str) -> Option<ConnectionLog> {
        let event = parse_connection(payload)?;
        let node = event
            .route
            .rsplit_once('[')
            .and_then(|(_, n)| n.strip_suffix(']'))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                if event.action == "代理" && event.error.is_empty() {
                    &event.route
                } else {
                    "未提供"
                }
            });
        let node = if matches!(event.action.as_str(), "直连" | "拦截") {
            "—"
        } else {
            node
        };
        let details = format!(
            "{}\n来源：{}\n原始规则：{}\n策略链路：{}{}\n入口 / 进程 / 连接 ID：未提供",
            self.identity,
            event.source,
            event.rule,
            event.route,
            if event.error.is_empty() {
                String::new()
            } else {
                format!(
                    "\n原因：{}",
                    crate::domain::redact::redact_text(&event.error)
                )
            }
        );
        Some(ConnectionLog {
            id: 0,
            at: event.at,
            protocol: event.protocol,
            address: event.address,
            source: event.source,
            action: event.action,
            rule: friendly_rule(&event.rule),
            profile: self.profile.clone(),
            node: node.into(),
            details,
        })
    }
}

#[derive(Deserialize)]
struct TrafficLine {
    up: u64,
    down: u64,
}
async fn traffic_loop(controller: Controller, sink: Arc<dyn LiveSink>) {
    loop {
        let _ = controller
            .stream_lines(&["traffic"], &[], |line| {
                if let Ok(t) = serde_json::from_slice::<TrafficLine>(line) {
                    sink.traffic(TrafficSample {
                        up: t.up,
                        down: t.down,
                    });
                }
            })
            .await;
        sink.traffic(TrafficSample::default());
        tokio::time::sleep(RECONNECT).await;
    }
}
#[derive(Deserialize)]
struct LogLine {
    payload: String,
}

struct ConnectionBatch {
    rows: Mutex<Vec<ConnectionLog>>,
    tx: mpsc::UnboundedSender<Internal>,
    epoch: u64,
}
impl ConnectionBatch {
    fn push(&self, row: ConnectionLog) {
        let mut rows = self.rows.lock().unwrap_or_else(|p| p.into_inner());
        rows.push(row);
        if rows.len() >= LOG_BATCH {
            let _ = self
                .tx
                .send(Internal::Connections(self.epoch, std::mem::take(&mut rows)));
        }
    }
    fn flush(&self) {
        let mut rows = self.rows.lock().unwrap_or_else(|p| p.into_inner());
        if !rows.is_empty() {
            let _ = self
                .tx
                .send(Internal::Connections(self.epoch, std::mem::take(&mut rows)));
        }
    }
}
impl Drop for ConnectionBatch {
    fn drop(&mut self) {
        self.flush();
    }
}

async fn connection_loop(
    controller: Controller,
    tx: mpsc::UnboundedSender<Internal>,
    epoch: u64,
    context: ConnectionContext,
) {
    let batch = ConnectionBatch {
        rows: Mutex::new(Vec::new()),
        tx: tx.clone(),
        epoch,
    };
    loop {
        let mut tick = tokio::time::interval(LOG_FLUSH);
        let stream = controller.stream_lines_ready(
            &["logs"],
            &[("level", "info")],
            || {
                let _ = tx.send(Internal::LogStreamStatus(epoch, "正在记录连接".into()));
            },
            |line| {
                if let Ok(line) = serde_json::from_slice::<LogLine>(line)
                    && let Some(row) = context.parse(&line.payload)
                {
                    batch.push(row);
                }
            },
        );
        {
            tokio::pin!(stream);
            loop {
                tokio::select! {
                    _ = &mut stream => break,
                    _ = tick.tick() => batch.flush(),
                }
            }
        }
        batch.flush();
        let _ = tx.send(Internal::LogStreamStatus(
            epoch,
            "采集暂时中断，正在重连（旧记录保留）".into(),
        ));
        tokio::time::sleep(RECONNECT).await;
    }
}
