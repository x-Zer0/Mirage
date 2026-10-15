use std::io;
use std::path::PathBuf;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// `%LOCALAPPDATA%` resolved through the shell, not the environment variable.
pub fn local_app_data() -> io::Result<PathBuf> {
    // SAFETY: on success the shell returns a NUL-terminated buffer that we own
    // and must release with CoTaskMemFree exactly once.
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)
            .map_err(io::Error::other)?;
        let path = raw.to_string().map_err(io::Error::other);
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(PathBuf::from)
    }
}
