//! Detects window areas Windows invalidated itself (restored from minimized,
//! dragged back from a screen edge, uncovered). The software renderer only
//! presents what Slint marked dirty and then validates the whole window, so
//! such exposures would keep stale pixels unless the next frame is full.

use std::cell::Cell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::GetUpdateRect;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::WM_PAINT;

const SUBCLASS_ID: usize = 0x4D50; // "MP"

thread_local! {
    static EXPOSED: Cell<bool> = const { Cell::new(false) };
}

/// Starts watching `hwnd` for OS invalidation. Idempotent per window.
pub fn watch(hwnd: HWND) {
    // SAFETY: `hwnd` is a live window of this thread; the subclass procedure
    // forwards every message unchanged.
    if !unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) }.as_bool() {
        tracing::warn!("window paint subclass failed");
    }
}

/// Returns and clears whether an OS exposure happened since the last call.
pub fn take_exposed() -> bool {
    EXPOSED.with(Cell::take)
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // `request_redraw` paints through RDW_INTERNALPAINT with no update region;
    // a non-empty one means Windows itself discarded window content.
    // SAFETY: called by the system with a valid window handle.
    if msg == WM_PAINT && unsafe { GetUpdateRect(hwnd, None, false) }.as_bool() {
        EXPOSED.with(|e| e.set(true));
    }
    // SAFETY: forwards to the next procedure in the chain.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
