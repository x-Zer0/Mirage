//! Known-folder paths cannot be redirected by inherited environment variables.
pub use mirage_ipc::known_folder::service_dir as install_dir;
use std::{io, path::PathBuf};
pub fn tun_home() -> io::Result<PathBuf> {
    Ok(mirage_ipc::known_folder::service_data()?.join("tun-home"))
}
pub fn core_exe() -> io::Result<PathBuf> {
    Ok(install_dir()?.join("mihomo.exe"))
}
pub fn service_exe() -> io::Result<PathBuf> {
    Ok(install_dir()?.join(mirage_ipc::service::EXE_NAME))
}
pub fn log_file() -> io::Result<PathBuf> {
    Ok(mirage_ipc::known_folder::service_data()?
        .join("logs")
        .join("mirage-svc.log"))
}
