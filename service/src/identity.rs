//! Minimal process identity helpers for the service (§10.4, §11.5).
//!
//! Deliberately separate from the app's copy: the service must not depend on
//! the UI crate, and it only needs the image path and a terminate call.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows::core::PWSTR;

#[derive(Debug, Clone)]
pub struct ProcessIdentity {
    pub image: String,
    pub created: u64,
}

pub fn identity(pid: u32) -> Option<ProcessIdentity> {
    let handle = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION.0).ok()?;
    let _ = &handle;
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
        image: String::from_utf16_lossy(&buf[..len as usize]),
        created: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
    })
}

pub fn terminate_matching(pid: u32, expected_created: u64, expected_image: &str) -> io::Result<()> {
    let handle = Handle::open(
        pid,
        PROCESS_TERMINATE.0 | PROCESS_QUERY_LIMITED_INFORMATION.0,
    )?;
    let (mut created, mut exit, mut kernel, mut user) = Default::default();
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    // SAFETY: one owned handle for identity verification and termination.
    unsafe {
        GetProcessTimes(handle.0, &mut created, &mut exit, &mut kernel, &mut user)
            .map_err(io::Error::other)?;
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .map_err(io::Error::other)?;
        let time = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        if time == expected_created
            && same_path(
                &String::from_utf16_lossy(&buf[..len as usize]),
                expected_image,
            )
        {
            TerminateProcess(handle.0, 1).map_err(io::Error::other)?;
        }
    }
    Ok(())
}
/// Path comparison used by the pipe: `\\?\` and case are both ignored.
pub fn same_path(a: &str, b: &str) -> bool {
    let normalise = |s: &str| {
        OsStr::new(s.strip_prefix(r"\\?\").unwrap_or(s))
            .to_string_lossy()
            .to_lowercase()
    };
    normalise(a) == normalise(b)
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

/// Encodes a UTF-8 string as a NUL-terminated UTF-16 buffer.
pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// String SID of the process owner (the elevated user during `install`).
pub fn current_user_sid() -> io::Result<String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: standard token query; the token handle and the SID string are
    // released before returning.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(io::Error::other)?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        // u64 storage keeps TOKEN_USER suitably aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let result = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .map_err(io::Error::other);
        let _ = CloseHandle(token);
        result?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(io::Error::other)?;
        let sid = text.to_string().map_err(io::Error::other);
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}
