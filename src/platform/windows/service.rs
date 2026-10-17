//! SCM start and the only UAC boundary: installing/updating/removing the helper.
use super::{
    autostart,
    registry::{self, Root},
    token,
};
use std::{io, path::Path};
use windows::{
    Win32::{
        Foundation::CloseHandle,
        System::{
            Services::*,
            Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        },
        UI::{
            Shell::{
                SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::PCWSTR,
};

pub const NAME: &str = "MirageService";
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn installed() -> bool {
    with_service(SERVICE_QUERY_STATUS, |_| Ok(())).is_ok()
}

pub fn start() -> io::Result<()> {
    with_service(SERVICE_START | SERVICE_QUERY_STATUS, |service| {
        // SAFETY: SCM service handle owned by with_service.
        match unsafe { StartServiceW(service, None) } {
            Ok(()) => Ok(()),
            Err(e) if e.code().0 as u32 & 0xffff == 1056 => Ok(()),
            Err(e) => Err(io::Error::other(e)),
        }
    })
}

/// Authenticate the pipe peer against SCM, which owns the service registration.
/// A normal user can query service status but cannot open a SYSTEM process.
pub fn verify_process(pid: u32) -> io::Result<()> {
    with_service(SERVICE_QUERY_STATUS, |service| {
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0;
        // SAFETY: the writable buffer has the size and alignment of the requested
        // status structure; the service handle remains owned by with_service.
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )
        };
        // SAFETY: buffer is valid and service has SERVICE_QUERY_STATUS access.
        unsafe { QueryServiceStatusEx(service, SC_STATUS_PROCESS_INFO, Some(buffer), &mut needed) }
            .map_err(io::Error::other)?;
        if status.dwCurrentState != SERVICE_RUNNING
            || status.dwServiceType != SERVICE_WIN32_OWN_PROCESS
            || pid == 0
            || status.dwProcessId != pid
        {
            return Err(io::Error::other("命名管道不属于正在运行的 Mirage 辅助服务"));
        }
        Ok(())
    })
}

fn with_service<T>(access: u32, f: impl FnOnce(SC_HANDLE) -> io::Result<T>) -> io::Result<T> {
    // SAFETY: each SCM handle is closed exactly once.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(io::Error::other)?;
        let service = OpenServiceW(scm, PCWSTR(wide(NAME).as_ptr()), access);
        let _ = CloseServiceHandle(scm);
        let service = service.map_err(io::Error::other)?;
        let result = f(service);
        let _ = CloseServiceHandle(service);
        result
    }
}

pub fn manage(install: bool, core_source: &Path) -> io::Result<()> {
    let client = std::env::current_exe()?;
    let helper = client.with_file_name("mirage-service.exe");
    if !helper.is_file() {
        return Err(io::Error::other(
            "缺少同目录的 mirage-service.exe，请使用完整安装包",
        ));
    }
    let args = if install {
        format!(
            "install --client \"{}\" --user-sid {} --core-source \"{}\"",
            client.display(),
            token::current_user_sid()?,
            core_source.display()
        )
    } else {
        "uninstall".to_owned()
    };
    elevate(&helper, &args)
}

fn elevate(helper: &Path, args: &str) -> io::Result<()> {
    let exe = wide(&helper.to_string_lossy());
    let args = wide(args);
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: buffers live through ShellExecute; returned process handle is closed.
    unsafe {
        ShellExecuteExW(&mut info)
            .map_err(|e| io::Error::from_raw_os_error((e.code().0 as u32 & 0xffff) as i32))?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1;
        let result = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        result.map_err(io::Error::other)?;
        if code != 0 {
            return Err(io::Error::other(
                "服务操作失败，请检查安装包完整性和管理员权限",
            ));
        }
    }
    Ok(())
}

/// An installed service belongs to one user and one application path.
fn matches_client() -> io::Result<bool> {
    let client =
        registry::read_string(Root::LocalMachine, r"SOFTWARE\Mirage\Service", "ClientExe")?;
    let sid = registry::read_string(Root::LocalMachine, r"SOFTWARE\Mirage\Service", "UserSid")?;
    let current = std::fs::canonicalize(std::env::current_exe()?)?;
    Ok(
        client.is_some_and(|p| p.eq_ignore_ascii_case(&current.to_string_lossy()))
            && sid.as_deref() == Some(token::current_user_sid()?.as_str()),
    )
}

/// Also catches replacement builds with the same application version number.
pub fn ready() -> bool {
    let check = || -> io::Result<bool> {
        if !installed() || !matches_client()? {
            return Ok(false);
        }
        let version =
            registry::read_string(Root::LocalMachine, r"SOFTWARE\Mirage\Service", "Version")?;
        if version.as_deref() != Some(env!("CARGO_PKG_VERSION")) {
            return Ok(false);
        }
        let installed = mirage_ipc::known_folder::service_dir()?.join("mirage-service.exe");
        let bundled = std::env::current_exe()?.with_file_name("mirage-service.exe");
        Ok(mirage_ipc::binary::hash_file(&installed)? == mirage_ipc::binary::hash_file(&bundled)?)
    };
    check().unwrap_or(false)
}

/// Called before Inno removes files. Cancellation/failure aborts uninstallation.
pub fn prepare_uninstall() -> io::Result<()> {
    if installed() && matches_client()? {
        manage(false, Path::new(""))?;
    }
    if autostart::is_current() {
        autostart::disable()?;
    }
    Ok(())
}
