use std::io;
use std::path::PathBuf;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::GUID;

/// `%LOCALAPPDATA%` resolved through the shell, not the environment variable.
pub fn local_app_data() -> io::Result<PathBuf> {
    known_folder(&FOLDERID_LocalAppData)
}

/// `%APPDATA%` (roaming), e.g. for the original ziyoumao client's data.
pub fn roaming_app_data() -> io::Result<PathBuf> {
    known_folder(&FOLDERID_RoamingAppData)
}

fn known_folder(id: &GUID) -> io::Result<PathBuf> {
    // SAFETY: on success the shell returns a NUL-terminated buffer that we own
    // and must release with CoTaskMemFree exactly once.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(io::Error::other)?;
        let path = raw.to_string().map_err(io::Error::other);
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(PathBuf::from)
    }
}
