#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cores;
mod domain;
mod logging;
mod platform;
mod storage;
mod subscriptions;
mod ui_bridge;

use anyhow::Context;
use platform::windows::single_instance::{Acquire, SingleInstance};
use storage::layout::Layout;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> anyhow::Result<()> {
    let autostart = std::env::args().any(|a| a == "--autostart");

    // §11.6: a second instance only wakes the first one up and exits.
    let instance = match SingleInstance::acquire().context("single-instance check")? {
        Acquire::Primary(instance) => instance,
        Acquire::AlreadyRunning => return Ok(()),
    };

    let layout = Layout::resolve().context("resolve data directory")?;
    layout.prepare().context("prepare data directory")?;
    let _log_guard = logging::init(&layout.logs).context("init logging")?;
    tracing::info!(version = VERSION, autostart, "Mirage starting");

    // §11.5: undo what an abnormal exit left behind before anything else runs.
    let recovery_notice = app::recovery::run(&layout);
    let core = app::spawn(layout.clone(), recovery_notice).context("start app runtime")?;
    platform::windows::session::on_session_end(core.session_end_hook());
    if let Some(path) = import_arg() {
        // Fresh queue, cannot be full.
        let _ = core.sender().try_send(app::Command::ImportLocalFile(path));
    }
    let ui = ui_bridge::Ui::build(&layout, VERSION, &core, autostart).context("build UI")?;
    instance.on_activate(ui.activator());

    slint::run_event_loop_until_quit().context("run event loop")?;

    tracing::info!("event loop finished, shutting down");
    drop(ui);
    core.shutdown();
    Ok(())
}

/// `--import <file>`: import a local config on start (used by file associations later).
fn import_arg() -> Option<std::path::PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--import" {
            return args.next().map(Into::into);
        }
    }
    None
}
