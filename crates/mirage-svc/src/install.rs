//! Elevated `install` / `uninstall` (DESIGN.md §11.7).
//!
//! The app launches `mirage-svc.exe install --client <path>` through
//! `ShellExecuteEx … "runas"`, so this code always runs with an administrator
//! token. It copies the helper and core out of the user-writable install
//! directory into `%ProgramFiles%\Mirage\mirage-svc`, records the SHA256 taken
//! here, locks both trees down, and only then creates the service.

use mirage_ipc::{
    atomic,
    binary::{EXPECTED_SHA256, hash_file},
};

use std::io;
use std::path::Path;
use windows::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SERVICE_ALL_ACCESS,
    SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, SERVICE_QUERY_STATUS,
    SERVICE_START, SERVICE_STATUS, SERVICE_STOP, SERVICE_WIN32_OWN_PROCESS, StartServiceW,
};
use windows::core::PCWSTR;

use crate::{acl, identity, paths, record};

use mirage_ipc::service::EXE_NAME;
pub use mirage_ipc::service::NAME as SERVICE_NAME;
const DISPLAY_NAME: &str = SERVICE_NAME;

const CORE_FILES: [&str; 5] = [
    "mihomo.exe",
    "GEOIP.metadb",
    "GEOSITE.dat",
    "GEOIP.dat",
    "ASN.mmdb",
];

pub fn install(client_exe: &Path, user_sid: &str, source_core: &Path) -> anyhow::Result<()> {
    if !client_exe.is_file()
        || client_exe
            .file_name()
            .is_none_or(|n| !n.eq_ignore_ascii_case("Mirage.exe"))
    {
        anyhow::bail!("--client 必须指向 Mirage.exe");
    }
    if !user_sid.starts_with("S-1-")
        || !user_sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
    {
        anyhow::bail!("安装用户 SID 不合法");
    }

    if hash_file(&source_core.join("mihomo.exe"))? != EXPECTED_SHA256 {
        anyhow::bail!("安装源不是固定 SHA256 的官方 Mihomo");
    }
    for name in CORE_FILES {
        if !source_core.join(name).is_file() {
            anyhow::bail!("安装源缺少 {name}");
        }
    }
    stop_service()?;
    let install_dir = paths::install_dir()?;
    let data_dir = mirage_ipc::known_folder::service_data()?;
    // Lock the parents before creating children or copying privileged payloads.
    secure_dir(install_dir.parent().unwrap(), user_sid)?;
    secure_dir(&install_dir, user_sid)?;
    secure_dir(&data_dir, user_sid)?;
    let tun_home = paths::tun_home()?;
    secure_dir(&tun_home, user_sid)?;
    secure_dir(&tun_home.join("logs"), user_sid)?;
    secure_dir(paths::log_file()?.parent().unwrap(), user_sid)?;
    copy_verified(&std::env::current_exe()?, &install_dir.join(EXE_NAME))?;
    for name in CORE_FILES {
        copy_verified(&source_core.join(name), &install_dir.join(name))?;
        if name != "mihomo.exe" {
            copy_verified(&install_dir.join(name), &tun_home.join(name))?;
        }
    }
    if hash_file(&install_dir.join("mihomo.exe"))? != EXPECTED_SHA256 {
        anyhow::bail!("复制后内核 SHA256 不匹配");
    }
    record::save(&record::InstallRecord {
        client_exe: std::fs::canonicalize(client_exe)?
            .to_string_lossy()
            .into_owned(),
        user_sid: user_sid.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        core_sha256: EXPECTED_SHA256.into(),
    })?;
    create_service()?;
    grant_start(user_sid)?;
    start_service()?;
    Ok(())
}

/// Reject junctions/symlinks before elevated writes, including pre-existing children.
fn secure_dir(path: &Path, sid: &str) -> anyhow::Result<()> {
    for ancestor in path.ancestors() {
        reject_reparse(ancestor)?;
    }
    std::fs::create_dir_all(path)?;
    acl::lock_tun_home(path, sid)?;
    secure_children(path, sid)?;
    Ok(())
}
fn secure_children(path: &Path, sid: &str) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        reject_reparse(&entry.path())?;
        acl::lock_tun_home(&entry.path(), sid)?;
        if entry.file_type()?.is_dir() {
            secure_children(&entry.path(), sid)?;
        }
    }
    Ok(())
}
fn reject_reparse(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_attributes() & 0x400 != 0 => {
            Err(io::Error::other("服务目录不能包含重解析点"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
pub fn uninstall() -> anyhow::Result<()> {
    stop_service()?;
    remove_service()?;
    record::remove();
    if let Ok(dir) = paths::install_dir() {
        // Only the dedicated subdirectory is removed; the app install tree
        // (`%ProgramFiles%\Mirage`) is left for the uninstaller.
        reject_reparse(&dir)?;
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
    }
    if let Ok(dir) = paths::tun_home() {
        reject_reparse(&dir)?;
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
    }
    Ok(())
}

/// Copies through a temp file, then replaces, so a running previous version
/// is not left half-written.
fn copy_verified(source: &Path, target: &Path) -> io::Result<()> {
    reject_reparse(target)?;
    let data = std::fs::read(source)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match atomic::write(target, &data) {
            Err(e)
                if e.kind() == io::ErrorKind::PermissionDenied
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            result => return result,
        }
    }
}
fn create_service() -> anyhow::Result<()> {
    let exe = paths::service_exe()?;
    let binary = format!("\"{}\"", exe.display());
    // SAFETY: SCM handles are closed on every path; strings are NUL-terminated.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CREATE_SERVICE)
            .map_err(|e| anyhow::anyhow!("无法打开服务管理器：{e}"))?;
        let name = identity::wide(SERVICE_NAME);
        let display = identity::wide(DISPLAY_NAME);
        let binary_w = identity::wide(&binary);
        let result = CreateServiceW(
            scm,
            PCWSTR(name.as_ptr()),
            PCWSTR(display.as_ptr()),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_DEMAND_START,
            SERVICE_ERROR_NORMAL,
            PCWSTR(binary_w.as_ptr()),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        );
        let created = match result {
            Ok(handle) => {
                let _ = CloseServiceHandle(handle);
                Ok(())
            }
            // A re-install over an existing service keeps the old definition;
            // the binary path and record were already refreshed above.
            Err(e) if is_already_exists(&e) => {
                let opened = OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_ALL_ACCESS);
                opened
                    .and_then(|service| {
                        let changed = windows::Win32::System::Services::ChangeServiceConfigW(
                            service,
                            SERVICE_WIN32_OWN_PROCESS,
                            SERVICE_DEMAND_START,
                            SERVICE_ERROR_NORMAL,
                            PCWSTR(binary_w.as_ptr()),
                            PCWSTR::null(),
                            None,
                            PCWSTR::null(),
                            PCWSTR(identity::wide("LocalSystem").as_ptr()),
                            PCWSTR::null(),
                            PCWSTR(display.as_ptr()),
                        );
                        let _ = CloseServiceHandle(service);
                        changed
                    })
                    .map_err(|e| anyhow::anyhow!("无法更新服务配置：{e}"))
            }
            Err(e) => Err(anyhow::anyhow!("无法创建服务：{e}")),
        };
        let _ = CloseServiceHandle(scm);
        created
    }
}

fn start_service() -> anyhow::Result<()> {
    // SAFETY: handles closed on both paths.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| anyhow::anyhow!("无法打开服务管理器：{e}"))?;
        let name = identity::wide(SERVICE_NAME);
        let result = OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_START).and_then(|service| {
            let started = StartServiceW(service, None);
            let _ = CloseServiceHandle(service);
            started
        });
        let _ = CloseServiceHandle(scm);
        match result {
            Ok(()) => Ok(()),
            // 1056 = already running, which is fine on re-install.
            Err(e) if (e.code().0 as u32 & 0xFFFF) == 1056 => Ok(()),
            Err(e) => Err(anyhow::anyhow!("无法启动服务：{e}")),
        }
    }
}

fn stop_service() -> anyhow::Result<()> {
    // SAFETY: handles closed on both paths.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| anyhow::anyhow!("无法打开服务管理器：{e}"))?;
        let name = identity::wide(SERVICE_NAME);
        let result = OpenServiceW(
            scm,
            PCWSTR(name.as_ptr()),
            SERVICE_QUERY_STATUS | SERVICE_STOP,
        )
        .and_then(|service| {
            let mut status = SERVICE_STATUS::default();
            let stopped = ControlService(service, SERVICE_CONTROL_STOP, &mut status)
                .and_then(|()| wait_stopped(service));
            let _ = CloseServiceHandle(service);
            stopped
        });
        let _ = CloseServiceHandle(scm);
        match result {
            Ok(()) => Ok(()),
            Err(e) if (e.code().0 as u32 & 0xFFFF) == 1060 || is_not_running(&e) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("无法停止服务：{e}")),
        }
    }
}

fn remove_service() -> anyhow::Result<()> {
    // SAFETY: handles closed on both paths.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| anyhow::anyhow!("无法打开服务管理器：{e}"))?;
        let name = identity::wide(SERVICE_NAME);
        let result =
            OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_ALL_ACCESS).and_then(|service| {
                let deleted = DeleteService(service);
                let _ = CloseServiceHandle(service);
                deleted
            });
        let _ = CloseServiceHandle(scm);
        match result {
            Ok(()) => Ok(()),
            Err(e) if is_missing(&e) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("无法删除服务：{e}")),
        }
    }
}

fn is_missing(e: &windows::core::Error) -> bool {
    (e.code().0 as u32 & 0xFFFF) == ERROR_SERVICE_DOES_NOT_EXIST.0
}

fn is_already_exists(e: &windows::core::Error) -> bool {
    // ERROR_SERVICE_EXISTS = 1073.
    (e.code().0 as u32 & 0xFFFF) == 1073
}

fn is_not_running(e: &windows::core::Error) -> bool {
    // ERROR_SERVICE_NOT_ACTIVE = 1062.
    (e.code().0 as u32 & 0xFFFF) == 1062
}

fn wait_stopped(service: windows::Win32::System::Services::SC_HANDLE) -> windows::core::Result<()> {
    use windows::Win32::System::Services::{QueryServiceStatus, SERVICE_STOPPED};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        let mut status = SERVICE_STATUS::default();
        // SAFETY: service is open with QUERY_STATUS and remains owned by caller.
        unsafe { QueryServiceStatus(service, &mut status) }?;
        if status.dwCurrentState == SERVICE_STOPPED {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(windows::core::Error::new(
                windows::core::HRESULT::from_win32(1460),
                "等待服务停止超时",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn grant_start(sid: &str) -> anyhow::Result<()> {
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
        System::Services::SetServiceObjectSecurity,
    };
    let sddl = identity::wide(&format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;CCLCRP;;;{sid})"));
    // SAFETY: SCM handles and descriptor are released on all paths.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)?;
        let opened = OpenServiceW(
            scm,
            PCWSTR(identity::wide(SERVICE_NAME).as_ptr()),
            SERVICE_ALL_ACCESS,
        );
        let _ = CloseServiceHandle(scm);
        let service = opened?;
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let result = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .and_then(|()| SetServiceObjectSecurity(service, DACL_SECURITY_INFORMATION, sd));
        if !sd.0.is_null() {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
        let _ = CloseServiceHandle(service);
        result?;
    }
    Ok(())
}
