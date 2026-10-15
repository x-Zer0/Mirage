//! Single instance per user (DESIGN.md §11.6): a named mutex marks the primary
//! instance; later instances signal a named auto-reset event and exit.

use super::token::current_user_sid;
use sha2::{Digest, Sha256};
use std::io;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForSingleObject,
};
use windows::core::HSTRING;

pub enum Acquire {
    Primary(SingleInstance),
    AlreadyRunning,
}

/// Holds the mutex and activation event for the lifetime of the process.
pub struct SingleInstance {
    mutex: HANDLE,
    event: HANDLE,
}

// SAFETY: kernel object handles may be used from any thread.
unsafe impl Send for SingleInstance {}

impl SingleInstance {
    pub fn acquire() -> io::Result<Acquire> {
        let suffix = user_suffix()?;
        let mutex_name = HSTRING::from(format!("Local\\Mirage-SingleInstance-{suffix}"));
        let event_name = HSTRING::from(format!("Local\\Mirage-Activate-{suffix}"));

        // SAFETY: plain Win32 object creation; every handle is closed on all paths.
        unsafe {
            let mutex = CreateMutexW(None, false, &mutex_name).map_err(io::Error::other)?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mutex);
                signal_primary(&event_name);
                return Ok(Acquire::AlreadyRunning);
            }
            match CreateEventW(None, false, false, &event_name) {
                Ok(event) => Ok(Acquire::Primary(Self { mutex, event })),
                Err(e) => {
                    let _ = CloseHandle(mutex);
                    Err(io::Error::other(e))
                }
            }
        }
    }

    /// Runs `activate` on a background thread every time another instance starts.
    pub fn on_activate(&self, activate: impl Fn() + Send + 'static) {
        let event = self.event.0 as usize;
        let spawned = std::thread::Builder::new()
            .name("mirage-activate".into())
            .spawn(move || {
                let event = HANDLE(event as *mut _);
                // SAFETY: the handle stays open until process exit (SingleInstance
                // lives in main), so the wait never observes a closed handle.
                while unsafe { WaitForSingleObject(event, INFINITE) }.0 == 0 {
                    activate();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "activation listener not started");
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: handles were created by this instance and are closed once.
        unsafe {
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.mutex);
        }
    }
}

fn signal_primary(event_name: &HSTRING) {
    // SAFETY: open/set/close of a named event; failure only means the primary
    // instance has not created its event yet, which is harmless.
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, event_name) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
        }
    }
}

/// Object names are per user; the SID is hashed so it never appears in names.
fn user_suffix() -> io::Result<String> {
    let sid = current_user_sid()?;
    let digest = Sha256::digest(sid.as_bytes());
    Ok(hex::encode(&digest[..8]))
}
