//! Process identity checks for leftover cores (DESIGN.md §10.4): a recorded
//! PID is only trusted when its creation time and image path still match.

use std::io;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
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

/// Every running process image name, without directory (DESIGN.md §11.8).
/// Unreadable entries are skipped: a conflict check must never fail a start.
pub fn image_names() -> Vec<String> {
    // SAFETY: snapshot iteration with a correctly sized PROCESSENTRY32W; the
    // snapshot handle is closed before returning.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return Vec::new();
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more {
            let name = String::from_utf16_lossy(&entry.szExeFile);
            let trimmed = name.trim_end_matches('\0');
            if !trimmed.is_empty() {
                out.push(trimmed.to_owned());
            }
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        out
    }
}

/// Image file name (no directory) of one PID; `None` when it is gone or denied.
pub fn image_name(pid: u32) -> Option<String> {
    let identity = identity(pid)?;
    Some(
        std::path::Path::new(&identity.image)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(identity.image),
    )
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
