//! Tray balloon notifications (DESIGN.md §11.4, §17.2). Slint's
//! `SystemTrayIcon` has no notification API, so this addresses the icon
//! Slint registered — its message-only window (class
//! `SlintSystemTrayWindow`, icon id 1) — and adds `NIF_INFO` to it. Only a
//! window owned by this process is used; nothing else is touched.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    NIF_INFO, NIIF_ERROR, NIIF_INFO, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE,
};
use windows::core::w;

/// Must match `i-slint-core` 1.18 `items/system_tray/windows.rs`.
const TRAY_UID: u32 = 1;

/// Shows a balloon on Mirage's tray icon. Returns false when the icon is not
/// found (the event is then only visible in the window).
pub fn show(title: &str, text: &str, error: bool) -> bool {
    let Some(hwnd) = own_tray_window() else {
        tracing::warn!("tray window not found; balloon skipped");
        return false;
    };
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_INFO,
        dwInfoFlags: if error { NIIF_ERROR } else { NIIF_INFO },
        ..Default::default()
    };
    copy_wide(&mut data.szInfoTitle, title);
    copy_wide(&mut data.szInfo, text);
    // SAFETY: `data` is fully initialised with cbSize set; only NIF_INFO
    // fields are read for NIM_MODIFY.
    let shown = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool();
    if !shown {
        tracing::warn!("tray balloon rejected by the shell");
    }
    shown
}

fn own_tray_window() -> Option<HWND> {
    let pid = std::process::id();
    let mut after = None;
    // SAFETY: plain window enumeration under HWND_MESSAGE.
    unsafe {
        while let Ok(hwnd) =
            FindWindowExW(Some(HWND_MESSAGE), after, w!("SlintSystemTrayWindow"), None)
        {
            let mut owner = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut owner));
            if owner == pid {
                return Some(hwnd);
            }
            after = Some(hwnd);
        }
    }
    None
}

/// Copies `text` truncated to the buffer, always NUL-terminated.
fn copy_wide<const N: usize>(dst: &mut [u16; N], text: &str) {
    for (slot, unit) in dst.iter_mut().zip(text.encode_utf16().take(N - 1)) {
        *slot = unit;
    }
}
