//! Install record and the client identity recorded at install time.
//!
//! `HKLM\SOFTWARE\Mirage\Service` is writable only by administrators, so the
//! values below are as trustworthy as the service itself. The pipe rejects a
//! client whose image path differs from `ClientExe`.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::core::PCWSTR;

const KEY: &str = r"SOFTWARE\Mirage\Service";
const VALUE_CLIENT: &str = "ClientExe";
const VALUE_USER: &str = "UserSid";
const VALUE_VERSION: &str = "Version";
/// SHA256 of the installed `mihomo.exe`; re-checked before every start.
const VALUE_CORE_SHA256: &str = "CoreSha256";

#[derive(Debug, Clone)]
pub struct InstallRecord {
    pub client_exe: String,
    pub user_sid: String,
    pub version: String,
    pub core_sha256: String,
}

pub fn load() -> std::io::Result<InstallRecord> {
    Ok(InstallRecord {
        client_exe: read_string(VALUE_CLIENT)?,
        user_sid: read_string(VALUE_USER)?,
        version: read_string(VALUE_VERSION)?,
        core_sha256: read_string(VALUE_CORE_SHA256)?,
    })
}

pub fn save(record: &InstallRecord) -> std::io::Result<()> {
    write_string(VALUE_CLIENT, &record.client_exe)?;
    write_string(VALUE_USER, &record.user_sid)?;
    write_string(VALUE_VERSION, &record.version)?;
    write_string(VALUE_CORE_SHA256, &record.core_sha256)
}

pub fn remove() {
    // SAFETY: deleting a subkey under HKLM; a missing key is the success case.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(wide(KEY).as_ptr()));
    }
}

fn read_string(name: &str) -> std::io::Result<String> {
    let name_w = wide(name);
    let mut bytes = 0u32;
    // First call asks for the size; a missing value reports ERROR_FILE_NOT_FOUND.
    // SAFETY: size query with a null buffer is the documented pattern.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(wide(KEY).as_ptr()),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    check(status)?;
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: the buffer matches the reported size in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(wide(KEY).as_ptr()),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    check(status)?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(&buf[..len]))
}

fn write_string(name: &str, value: &str) -> std::io::Result<()> {
    let mut key = HKEY::default();
    // SAFETY: creates or opens the key; the handle is closed below.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(wide(KEY).as_ptr()),
            None,
            PCWSTR::null(),
            Default::default(),
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    check(status)?;
    let data: Vec<u16> = OsStr::new(value).encode_wide().chain(Some(0)).collect();
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(data.as_ptr().cast(), data.len() * 2) };
    // SAFETY: `key` was opened above; the slice encodes a NUL-terminated string.
    let status =
        unsafe { RegSetValueExW(key, PCWSTR(wide(name).as_ptr()), None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    check(status)
}

fn check(status: WIN32_ERROR) -> std::io::Result<()> {
    if status.0 == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status.0 as i32))
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}
