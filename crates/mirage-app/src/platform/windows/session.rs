//! Logoff/shutdown notification (DESIGN.md §11.5). A hidden top-level window
//! (message-only windows do not receive WM_ENDSESSION) runs the cleanup hook
//! synchronously before Windows ends the process.

use std::sync::OnceLock;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
    WINDOW_EX_STYLE, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::w;

type Hook = Box<dyn Fn() + Send + Sync>;
static HOOK: OnceLock<Hook> = OnceLock::new();

/// Installs `hook` (once per process); it runs on the listener thread when the
/// session ends and should return only after cleanup is done.
pub fn on_session_end(hook: impl Fn() + Send + Sync + 'static) {
    if HOOK.set(Box::new(hook)).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("mirage-session".into())
        .spawn(|| {
            if let Err(e) = message_loop() {
                tracing::warn!(error = %e, "session listener stopped");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "session listener not started");
    }
}

fn message_loop() -> windows::core::Result<()> {
    // SAFETY: standard window class registration and message pump on this
    // thread; the window is never shown and lives until process exit.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("MirageSessionWatcher"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("MirageSessionWatcher"),
            w!("Mirage"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0
                && let Some(hook) = HOOK.get()
            {
                tracing::info!("session ending, cleaning up");
                hook();
            }
            LRESULT(0)
        }
        // SAFETY: default handling for every other message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
