//! Application core actor (DESIGN.md §6.5). M0 only owns the runtime thread and
//! the command channel; use cases are added from M1 on.

use std::thread::JoinHandle;
use tokio::sync::mpsc;

const COMMAND_QUEUE: usize = 64;

#[derive(Debug)]
pub enum Command {
    Quit,
}

pub struct CoreHandle {
    tx: mpsc::Sender<Command>,
    thread: JoinHandle<()>,
}

impl CoreHandle {
    pub fn sender(&self) -> mpsc::Sender<Command> {
        self.tx.clone()
    }

    /// Asks the actor to finish its shutdown work and waits for the runtime thread.
    pub fn shutdown(self) {
        // A closed channel means the actor already stopped; nothing left to do.
        let _ = self.tx.blocking_send(Command::Quit);
        if self.thread.join().is_err() {
            tracing::error!("app runtime thread panicked");
        }
    }
}

pub fn spawn() -> std::io::Result<CoreHandle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("mirage-rt")
        .enable_all()
        .build()?;
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
    let thread = std::thread::Builder::new()
        .name("mirage-core".into())
        .spawn(move || runtime.block_on(run(rx)))?;
    Ok(CoreHandle { tx, thread })
}

async fn run(mut rx: mpsc::Receiver<Command>) {
    tracing::debug!("app core started");
    while let Some(cmd) = rx.recv().await {
        tracing::debug!(?cmd, "command");
        if matches!(cmd, Command::Quit) {
            break;
        }
    }
    tracing::debug!("app core stopped");
}
