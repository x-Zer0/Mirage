#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod domain;
mod logging;
mod platform;
mod storage;
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

    let core = app::spawn().context("start app runtime")?;
    let ui =
        ui_bridge::Ui::build(&layout, VERSION, core.sender(), autostart).context("build UI")?;
    instance.on_activate(ui.activator());

    slint::run_event_loop_until_quit().context("run event loop")?;

    tracing::info!("event loop finished, shutting down");
    drop(ui);
    core.shutdown();
    Ok(())
}
