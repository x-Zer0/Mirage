//! SCM lifecycle. Async pipe waits observe STOP even with no connected client.
use crate::{core::CoreShell, identity, install::SERVICE_NAME, log, paths, pipe};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use windows::{
    Win32::{Foundation::NO_ERROR, System::Services::*},
    core::{PCWSTR, PWSTR},
};

static STOPPING: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicUsize = AtomicUsize::new(0);

pub fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}
pub async fn wait_stop() {
    while !stopping() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub fn dispatch() -> anyhow::Result<()> {
    let mut name = identity::wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR::null(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: terminated table lives throughout the blocking dispatcher call.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }?;
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let result = run();
    if result.is_err() {
        log::error("service stopped with an error");
    }
    report(SERVICE_STOPPED, if result.is_err() { 1 } else { 0 });
}

fn run() -> anyhow::Result<()> {
    let name = identity::wide(SERVICE_NAME);
    // SAFETY: handler has the SCM ABI and process lifetime.
    let status =
        unsafe { RegisterServiceCtrlHandlerExW(PCWSTR(name.as_ptr()), Some(control), None) }?;
    STATUS.store(status.0 as usize, Ordering::SeqCst);
    report(SERVICE_START_PENDING, 0);
    log::init(paths::log_file()?);
    let shell = Arc::new(CoreShell::default());
    shell.clean_leftover();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    report(SERVICE_RUNNING, 0);
    let result = runtime.block_on(pipe::serve(shell.clone()));
    let _ = shell.stop();
    result
}

unsafe extern "system" fn control(
    code: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    if matches!(
        code,
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN | SERVICE_CONTROL_PRESHUTDOWN
    ) {
        STOPPING.store(true, Ordering::SeqCst);
        report(SERVICE_STOP_PENDING, 0);
    }
    NO_ERROR.0
}

fn report(state: SERVICE_STATUS_CURRENT_STATE, error: u32) {
    let raw = STATUS.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    let pending = state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING;
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_PRESHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: error,
        dwCheckPoint: u32::from(pending),
        dwWaitHint: if pending { 90_000 } else { 0 },
        ..Default::default()
    };
    // SAFETY: handle was registered in this service process.
    let _ = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(raw as *mut _), &status) };
}
