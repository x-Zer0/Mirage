//! Private directory ACL (DESIGN.md §14.2): protected DACL granting full control
//! to the current user and SYSTEM only. Children inherit it.

use super::token::current_user_sid;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW};
use windows::core::PCWSTR;

pub fn restrict_to_owner(dir: &Path) -> io::Result<()> {
    let sid = current_user_sid()?;
    // P = protected (no inheritance from parent); OICI = inherit to files and dirs.
    let sddl = format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)");
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
        let applied = SetFileSecurityW(PCWSTR(path_w.as_ptr()), DACL_SECURITY_INFORMATION, sd);
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
