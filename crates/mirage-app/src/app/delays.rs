//! Bounded per-node latency queue, result lifecycle and thirty-minute expiry.
use super::state::RuntimeStatus;
use super::{AppCore, Internal};
use crate::cores::mihomo::controller::{Controller, DelayOutcome, NodeDelay, NodeTestState};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use std::collections::HashSet;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

const MAX_PARALLEL: usize = 16;

#[derive(Default)]
pub(super) struct DelayTests {
    pub batch: u64,
    /// Invalidates group snapshots only when the runtime configuration changes.
    pub groups_epoch: u64,
    task: Option<JoinHandle<()>>,
}

pub(super) enum DelayEvent {
    Started(String),
    Finished(String, AppResult<u32>),
    Done,
}

impl AppCore {
    pub(super) fn test_delay(&mut self, group: String) {
        if self.view.testing_group.is_some() || self.view.runtime != RuntimeStatus::Running {
            return;
        }
        let Some(running) = &self.running else { return };
        let Some(selected) = self.view.groups.iter().find(|g| g.name == group) else {
            return;
        };
        let mut unique = HashSet::new();
        let nodes: Vec<_> = selected
            .nodes
            .iter()
            .map(|n| n.name.clone())
            .filter(|name| unique.insert(name.clone()))
            .collect();
        if nodes.is_empty() {
            return;
        }
        let controller = running.controller.clone();
        self.delay_tests.batch += 1;
        let batch = self.delay_tests.batch;
        for node in self.view.groups.iter_mut().flat_map(|g| &mut g.nodes) {
            if unique.contains(&node.name) {
                node.test_state = NodeTestState::Queued;
            }
        }
        self.view.testing_group = Some(group);
        self.view.testing_completed = 0;
        self.view.testing_total = nodes.len();
        self.node_delays_changed();
        self.delay_tests.task = Some(tokio::spawn(run_tests(
            controller,
            nodes,
            self.internal_tx.clone(),
            self.generation,
            batch,
        )));
    }

    pub(super) fn on_node_delay(&mut self, generation: u64, batch: u64, event: DelayEvent) {
        if generation != self.generation
            || batch != self.delay_tests.batch
            || self.view.testing_group.is_none()
        {
            return;
        }
        match event {
            DelayEvent::Started(name) => {
                for node in self.view.groups.iter_mut().flat_map(|g| &mut g.nodes) {
                    if node.name == name {
                        node.test_state = NodeTestState::Testing;
                    }
                }
            }
            DelayEvent::Finished(name, result) => {
                let delay = measured_delay(result);
                for node in self.view.groups.iter_mut().flat_map(|g| &mut g.nodes) {
                    if node.name == name {
                        node.delay = Some(delay.clone());
                        node.test_state = NodeTestState::Idle;
                    }
                }
                self.view.testing_completed += 1;
            }
            DelayEvent::Done => self.finish_delay_tests(),
        }
        self.node_delays_changed();
    }

    fn finish_delay_tests(&mut self) {
        self.delay_tests.task = None;
        self.view.testing_group = None;
        // A worker panic must not leave a row permanently waiting or testing.
        for node in self.view.groups.iter_mut().flat_map(|g| &mut g.nodes) {
            if node.test_state != NodeTestState::Idle {
                node.test_state = NodeTestState::Idle;
                node.delay = Some(measured_delay(Err(AppError::internal(
                    "mihomo.controller",
                    "测速任务未正常完成，请重试",
                ))));
            }
        }
        self.view.testing_completed = self.view.testing_total;
    }

    pub(super) fn reset_delay_tests(&mut self) {
        self.delay_tests.batch += 1;
        self.delay_tests.groups_epoch += 1;
        if let Some(task) = self.delay_tests.task.take() {
            task.abort();
        }
        self.view.testing_group = None;
        self.view.testing_completed = 0;
        self.view.testing_total = 0;
        for node in self.view.groups.iter_mut().flat_map(|g| &mut g.nodes) {
            node.test_state = NodeTestState::Idle;
            node.delay = None;
        }
        self.node_delays_changed();
    }

    pub(super) fn expire_node_delays(&mut self) {
        let mut changed = false;
        for node in self.view.groups.iter_mut().flat_map(|g| &mut g.nodes) {
            if node.delay.as_ref().is_some_and(|d| !d.is_fresh()) {
                node.delay = None;
                changed = true;
            }
        }
        if changed {
            self.node_delays_changed();
        }
    }

    fn node_delays_changed(&mut self) {
        self.view.groups_revision += 1;
        self.view_dirty = true;
    }
}

fn measured_delay(result: AppResult<u32>) -> NodeDelay {
    let (outcome, detail) = match result {
        Ok(ms) => (DelayOutcome::Reachable(ms), String::new()),
        Err(e) => {
            let outcome = if e.kind == AppErrorKind::Timeout {
                DelayOutcome::Timeout
            } else {
                DelayOutcome::Failed
            };
            (outcome, e.user_message())
        }
    };
    NodeDelay {
        outcome,
        detail,
        measured_at: Instant::now(),
    }
}

async fn run_tests(
    controller: Controller,
    nodes: Vec<String>,
    tx: mpsc::UnboundedSender<Internal>,
    generation: u64,
    batch: u64,
) {
    let mut pending = nodes.into_iter();
    let mut running = JoinSet::new();
    loop {
        while running.len() < MAX_PARALLEL {
            let Some(name) = pending.next() else { break };
            if tx
                .send(Internal::NodeDelay(
                    generation,
                    batch,
                    DelayEvent::Started(name.clone()),
                ))
                .is_err()
            {
                return;
            }
            let controller = controller.clone();
            running.spawn(async move {
                let result = controller.test_node_delay(&name).await;
                (name, result)
            });
        }
        match running.join_next().await {
            Some(Ok((name, result))) => {
                if tx
                    .send(Internal::NodeDelay(
                        generation,
                        batch,
                        DelayEvent::Finished(name, result),
                    ))
                    .is_err()
                {
                    return;
                }
            }
            Some(Err(e)) => tracing::warn!(error = %e, "node delay worker failed"),
            None => break,
        }
    }
    let _ = tx.send(Internal::NodeDelay(generation, batch, DelayEvent::Done));
}
