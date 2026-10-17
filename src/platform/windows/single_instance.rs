//! Per-user instance, activation and orderly installer shutdown events.
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

pub struct SingleInstance {
    mutex: HANDLE,
    event: HANDLE,
    quit_event: HANDLE,
}

// SAFETY: kernel object handles may be used from any thread.
unsafe impl Send for SingleInstance {}

impl SingleInstance {
    pub fn acquire() -> io::Result<Acquire> {
        let suffix = user_suffix()?;
        let mutex_name = object_name("SingleInstance", &suffix);
        let event_name = object_name("Activate", &suffix);
        // SAFETY: each created handle is owned here and closed on error or Drop.
        unsafe {
            let mutex = CreateMutexW(None, false, &mutex_name).map_err(io::Error::other)?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mutex);
                signal_primary(&event_name);
                return Ok(Acquire::AlreadyRunning);
            }
            let events = create_events(&event_name, &object_name("Quit", &suffix));
            match events {
                Ok((event, quit_event)) => Ok(Acquire::Primary(Self {
                    mutex,
                    event,
                    quit_event,
                })),
                Err(e) => {
                    let _ = CloseHandle(mutex);
                    Err(e)
                }
            }
        }
    }

    pub fn on_activate(&self, activate: impl Fn() + Send + 'static) {
        listen(self.event, "mirage-activate", activate);
    }

    /// Bypasses close-to-tray so installer shutdown follows main's cleanup path.
    pub fn on_quit(&self, quit: impl Fn() + Send + 'static) {
        listen(self.quit_event, "mirage-installer-quit", quit);
    }
}

fn create_events(activate: &HSTRING, quit: &HSTRING) -> io::Result<(HANDLE, HANDLE)> {
    // SAFETY: names outlive creation; handles are transferred or closed on error.
    unsafe {
        let event = CreateEventW(None, false, false, activate).map_err(io::Error::other)?;
        match CreateEventW(None, false, false, quit) {
            Ok(quit_event) => Ok((event, quit_event)),
            Err(e) => {
                let _ = CloseHandle(event);
                Err(io::Error::other(e))
            }
        }
    }
}

fn listen(event: HANDLE, name: &str, callback: impl Fn() + Send + 'static) {
    let event = event.0 as usize;
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let event = HANDLE(event as *mut _);
            // SAFETY: SingleInstance retains the handle for the process lifetime.
            while unsafe { WaitForSingleObject(event, INFINITE) }.0 == 0 {
                callback();
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "instance listener not started");
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: each owned handle is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.quit_event);
            let _ = CloseHandle(self.mutex);
        }
    }
}

fn signal_primary(event_name: &HSTRING) {
    // SAFETY: opens and closes a temporary handle; absent primary is harmless.
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, event_name) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
        }
    }
}

fn object_name(kind: &str, suffix: &str) -> HSTRING {
    HSTRING::from(format!("Local\\Mirage-{kind}-{suffix}"))
}

fn user_suffix() -> io::Result<String> {
    let digest = Sha256::digest(current_user_sid()?.as_bytes());
    Ok(hex::encode(&digest[..8]))
}

/// Waits for normal exit, including proxy restoration. Timeout aborts setup.
pub fn request_shutdown() -> io::Result<()> {
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let suffix = user_suffix()?;
    let name = object_name("SingleInstance", &suffix);
    let event = object_name("Quit", &suffix);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        // SAFETY: probes existence without taking ownership; closes immediately.
        match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &name) } {
            Ok(handle) => unsafe {
                let _ = CloseHandle(handle);
            },
            Err(e) if e.code().0 as u32 & 0xffff == ERROR_FILE_NOT_FOUND.0 => return Ok(()),
            Err(e) => return Err(io::Error::other(e)),
        }
        signal_primary(&event);
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::other("请从托盘退出 Mirage，再重试安装或卸载"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
