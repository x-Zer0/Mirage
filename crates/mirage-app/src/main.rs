#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cores;
mod domain;
mod logging;
mod net;
mod overrides;
mod platform;
mod providers;
mod storage;
mod subscriptions;
mod ui_bridge;

use anyhow::Context;
use platform::windows::single_instance::{Acquire, SingleInstance};
use storage::layout::Layout;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--quit" || a == "--prepare-uninstall") {
        platform::windows::single_instance::request_shutdown()?;
        app::recovery::prepare_install(&Layout::resolve()?)?;
        if std::env::args().any(|a| a == "--prepare-uninstall") {
            platform::windows::service::prepare_uninstall()?;
        }
        return Ok(());
    }
    let autostart = std::env::args().any(|a| a == "--autostart");
    let check_service = std::env::args().any(|a| a == "--check-service");

    // §11.6: a second instance only wakes the first one up and exits.
    let instance = match SingleInstance::acquire().context("single-instance check")? {
        Acquire::Primary(instance) => instance,
        Acquire::AlreadyRunning if check_service => anyhow::bail!("请先退出 Mirage 再检查服务"),
        Acquire::AlreadyRunning => return Ok(()),
    };

    let layout = Layout::resolve().context("resolve data directory")?;
    layout.prepare().context("prepare data directory")?;
    // M25: 日志初始化失败时降级为 stderr 并通知用户
    let _log_guard = match logging::init(&layout.logs) {
        Ok(guard) => Some(guard),
        Err(e) => {
            // 降级为 stderr（release 没有控制台时会被忽略，但不会崩溃）
            let _ = tracing_subscriber::fmt()
                .with_target(true)
                .with_ansi(false)
                .try_init();
            // 弹窗通知用户
            show_warning(&format!("日志系统初始化失败，日志将不会写入文件：{e}"));
            None
        }
    };
    tracing::info!(version = VERSION, autostart, "Mirage starting");
    if check_service {
        return check_service_connections();
    }
    // ring keeps the build free of the aws-lc C/NASM toolchain (§12.1).
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        tracing::debug!("rustls crypto provider already installed");
    }

    // §11.5: undo what an abnormal exit left behind before anything else runs.
    let recovery_notice = app::recovery::run(&layout);
    let core = app::spawn(layout.clone(), recovery_notice).context("start app runtime")?;
    platform::windows::session::on_session_end(core.session_end_hook());
    if let Some(path) = import_arg() {
        // Fresh queue, cannot be full.
        let _ = core.sender().try_send(app::Command::ImportLocalFile(path));
    }
    let ui = ui_bridge::Ui::build(&layout, VERSION, &core, autostart).context("build UI")?;
    instance.on_activate(ui.activator(core.sender()));
    instance.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

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

/// Reconnects to expose stale async pipe state after an earlier disconnect.
fn check_service_connections() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        for attempt in 1..=3 {
            cores::mihomo::service::check()
                .await
                .with_context(|| format!("辅助服务第 {attempt} 次连接检查失败"))?;
            tracing::info!(attempt, "service connection check passed");
        }
        Ok(())
    })
}

/// M25: 弹出警告消息框
fn show_warning(msg: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};

    let wide_msg: Vec<u16> = OsStr::new(msg).encode_wide().chain(Some(0)).collect();
    let wide_title: Vec<u16> = OsStr::new("Mirage").encode_wide().chain(Some(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            windows::core::PCWSTR(wide_msg.as_ptr()),
            windows::core::PCWSTR(wide_title.as_ptr()),
            MB_OK | MB_ICONWARNING,
        );
    }
}
