//! Process identity checks for leftover cores (DESIGN.md §10.4): a recorded
//! PID is only trusted when its creation time and image path still match.

use std::io;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows::core::PWSTR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// Creation time as a FILETIME value (100 ns ticks since 1601).
    pub created: u64,
    pub image: String,
}

impl ProcessIdentity {
    pub fn matches(&self, other: &Self) -> bool {
        self.created == other.created && self.image.eq_ignore_ascii_case(&other.image)
    }
}

/// `None` when the process no longer exists or cannot be queried.
pub fn identity(pid: u32) -> Option<ProcessIdentity> {
    let handle = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION.0).ok()?;
    let mut created = FILETIME::default();
    let (mut exit, mut kernel, mut user) = Default::default();
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: valid process handle; output pointers reference live locals and
    // `len` matches the buffer capacity.
    unsafe {
        GetProcessTimes(handle.0, &mut created, &mut exit, &mut kernel, &mut user).ok()?;
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
    }
    Some(ProcessIdentity {
        created: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        image: String::from_utf16_lossy(&buf[..len as usize]),
    })
}

pub fn terminate(pid: u32) -> io::Result<()> {
    let handle = Handle::open(pid, PROCESS_TERMINATE.0)?;
    // SAFETY: handle opened with PROCESS_TERMINATE for exactly this PID.
    unsafe { TerminateProcess(handle.0, 1) }.map_err(io::Error::other)
}

struct Handle(HANDLE);

impl Handle {
    fn open(pid: u32, access: u32) -> io::Result<Self> {
        use windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS;
        // SAFETY: plain OpenProcess; the handle is closed by Drop.
        unsafe { OpenProcess(PROCESS_ACCESS_RIGHTS(access), false, pid) }
            .map(Self)
            .map_err(io::Error::other)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
