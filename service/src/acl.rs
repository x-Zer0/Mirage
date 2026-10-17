//! Directory ACLs for the two service-owned trees (DESIGN.md §11.7).
//!
//! * `%ProgramFiles%\Mirage\service` is writable by Administrators only. The
//!   installing user gets read/execute so the app can check its own version.
//! * `%ProgramData%\Mirage\tun-home` is fully controlled by SYSTEM and
//!   Administrators; the installing user is read-only, which is what lets the
//!   UI read the core log without being able to plant a config.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SetFileSecurityW,
};
use windows::core::PCWSTR;

const SYSTEM: &str = "SY";
const ADMINISTRATORS: &str = "BA";

/// SYSTEM and Administrators write; the installing user reads.
pub fn lock_tun_home(dir: &Path, user_sid: &str) -> io::Result<()> {
    let sddl = format!(
        "O:BAD:P(A;OICI;FA;;;{SYSTEM})(A;OICI;FA;;;{ADMINISTRATORS})(A;OICI;GRGX;;;{user_sid})"
    );
    apply(dir, &sddl)
}

/// `D:P` protects the DACL from inheritance; `OICI` propagates to children.
fn apply(dir: &Path, sddl: &str) -> io::Result<()> {
    let sddl_w = wide(sddl.as_ref());
    let path_w = wide(dir.as_os_str());
    // SAFETY: both strings are NUL-terminated and outlive the calls; the
    // descriptor allocated by the conversion is released with LocalFree.
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl_w.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(io::Error::other)?;
        let applied = SetFileSecurityW(
            PCWSTR(path_w.as_ptr()),
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            sd,
        );
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        if applied.as_bool() {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}
