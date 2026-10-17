//! Live data from the running core (DESIGN.md §6.5, §17.4): `/traffic`
//! and `/logs` are only streamed while the main window is visible, so a
//! hidden window costs no controller traffic and no UI redraws.
//!
//! The streams bypass the actor's state snapshot: samples and log batches go
//! straight to a [`LiveSink`] (the UI bridge), throttled at the source.

use super::AppCore;
use super::state::TrafficSample;
use crate::cores::mihomo::controller::Controller;
use crate::domain::redact::redact_text;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Batched log lines are flushed at most this often.
const LOG_FLUSH: Duration = Duration::from_millis(500);
/// Upper bound per batch; a log storm cannot flood the UI queue.
const LOG_BATCH: usize = 200;
/// Delay before reopening a stream that ended while the core still runs.
const RECONNECT: Duration = Duration::from_secs(2);

/// One core log line for the diagnostics page (redacted, §14.3).
#[derive(Debug, Clone)]
pub struct UiLogLine {
    pub level: String,
    pub text: String,
}

/// Receiver of live data; implemented by the UI bridge.
pub trait LiveSink: Send + Sync + std::fmt::Debug {
    fn traffic(&self, sample: TrafficSample);
    fn log_lines(&self, lines: Vec<UiLogLine>);
}

#[derive(Default)]
pub(super) struct LiveStreams {
    sink: Option<Arc<dyn LiveSink>>,
    visible: bool,
    tasks: Vec<JoinHandle<()>>,
}

impl LiveStreams {
    pub fn set_sink(&mut self, sink: Arc<dyn LiveSink>) {
        self.sink = Some(sink);
    }

    /// Quit: the actor stops; tasks must not outlive the core.
    pub fn set_window_hidden_for_quit(&mut self) {
        self.visible = false;
        self.stop();
    }

    fn stop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        if let Some(sink) = &self.sink {
            sink.traffic(TrafficSample::default());
        }
    }
}

impl AppCore {
    pub(super) fn set_window_visible(&mut self, visible: bool) {
        if self.live.visible == visible {
            return;
        }
        self.live.visible = visible;
        self.sync_live_streams();
    }

    /// Starts or stops the streams to match "window visible and core up".
    /// Called after every state change that affects either side.
    pub(super) fn sync_live_streams(&mut self) {
        let wanted = self.live.visible && self.view.runtime.is_up() && self.running.is_some();
        let streaming = !self.live.tasks.is_empty();
        if wanted && !streaming {
            let (Some(sink), Some(running)) = (self.live.sink.clone(), &self.running) else {
                return;
            };
            let controller = running.controller.clone();
            tracing::debug!("live streams started");
            self.live.tasks = vec![
                tokio::spawn(traffic_loop(controller.clone(), sink.clone())),
                tokio::spawn(log_loop(controller, sink)),
            ];
        } else if !wanted && streaming {
            tracing::debug!("live streams stopped");
            self.live.stop();
        }
    }

    /// The core changed (stop, restart, profile switch): streams must reconnect.
    pub(super) fn restart_live_streams(&mut self) {
        self.live.stop();
        self.sync_live_streams();
    }
}

#[derive(Deserialize)]
struct TrafficLine {
    up: u64,
    down: u64,
}

/// `/traffic` sends one line per second; each becomes one sample.
async fn traffic_loop(controller: Controller, sink: Arc<dyn LiveSink>) {
    loop {
        let result = controller
            .stream_lines(&["traffic"], &[], |line| {
                if let Ok(t) = serde_json::from_slice::<TrafficLine>(line) {
                    sink.traffic(TrafficSample {
                        up: t.up,
                        down: t.down,
                    });
                }
            })
            .await;
        if let Err(e) = result {
            tracing::debug!(error = %e, "traffic stream ended");
        }
        sink.traffic(TrafficSample::default());
        tokio::time::sleep(RECONNECT).await;
    }
}

#[derive(Deserialize)]
struct LogLine {
    #[serde(rename = "type")]
    level: String,
    payload: String,
}

/// `/logs?level=warning`, batched so a burst costs one UI update per flush.
async fn log_loop(controller: Controller, sink: Arc<dyn LiveSink>) {
    let level = super::runtime::CORE_LOG_LEVEL;
    loop {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<UiLogLine>(LOG_BATCH * 2);
        let flusher = {
            let sink = sink.clone();
            tokio::spawn(async move {
                let mut batch = Vec::new();
                let mut tick = tokio::time::interval(LOG_FLUSH);
                loop {
                    tokio::select! {
                        line = rx.recv() => match line {
                            Some(line) if batch.len() < LOG_BATCH => batch.push(line),
                            Some(_) => {}
                            None => break,
                        },
                        _ = tick.tick() => {
                            if !batch.is_empty() {
                                sink.log_lines(std::mem::take(&mut batch));
                            }
                        }
                    }
                }
                if !batch.is_empty() {
                    sink.log_lines(batch);
                }
            })
        };
        let result = controller
            .stream_lines(&["logs"], &[("level", level)], |line| {
                if let Ok(l) = serde_json::from_slice::<LogLine>(line) {
                    // A full queue drops lines instead of blocking the stream.
                    let _ = tx.try_send(UiLogLine {
                        level: l.level,
                        text: redact_text(&l.payload).into_owned(),
                    });
                }
            })
            .await;
        drop(tx);
        let _ = flusher.await;
        if let Err(e) = result {
            tracing::debug!(error = %e, "log stream ended");
        }
        tokio::time::sleep(RECONNECT).await;
    }
}
