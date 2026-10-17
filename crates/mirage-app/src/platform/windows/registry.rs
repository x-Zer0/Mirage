//! Small `RegQueryValueExW`/`RegSetValueExW` helper (DESIGN.md §11.6, §18).
//! Used for the autostart Run entry and for the Windows version in the
//! diagnostics bundle.

use std::io;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY,
    REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::PCWSTR;

/// Hive to operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    CurrentUser,
    LocalMachine,
}

impl Root {
    fn hkey(self) -> HKEY {
        match self {
            Self::CurrentUser => HKEY_CURRENT_USER,
            Self::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }
}

/// Reads a `REG_SZ`; `None` when the subkey or value is absent.
pub fn read_string(root: Root, subkey: &str, value: &str) -> io::Result<Option<String>> {
    // HKLM is read through the 64-bit view so a 32-bit build still sees the
    // real OS values (§18 Windows version).
    let Some(key) = Key::open(root, subkey, KEY_QUERY_VALUE | KEY_WOW64_64KEY)? else {
        return Ok(None);
    };
    let name = wide(value);
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    // SAFETY: sizing query with a null buffer; `key` is open and `name` alive.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    match status {
        ERROR_FILE_NOT_FOUND => return Ok(None),
        ERROR_SUCCESS => {}
        other => return Err(reg_error("RegQueryValueExW", value, other.0)),
    }
    if kind == REG_SZ && size >= 2 {
        let mut buf = vec![0u8; size as usize];
        // SAFETY: `buf` is exactly the size the sizing call reported.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(buf.as_mut_ptr()),
                Some(&mut size),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(reg_error("RegQueryValueExW", value, status.0));
        }
        return Ok(Some(from_utf16_bytes(&buf[..size as usize])));
    }
    Ok(None)
}

/// Reads a `REG_DWORD`; `None` when absent or of another type.
pub fn read_dword(root: Root, subkey: &str, value: &str) -> io::Result<Option<u32>> {
    let Some(key) = Key::open(root, subkey, KEY_QUERY_VALUE | KEY_WOW64_64KEY)? else {
        return Ok(None);
    };
    let name = wide(value);
    let mut kind = windows::Win32::System::Registry::REG_VALUE_TYPE::default();
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `data` is a live u32 and `size` matches its size exactly.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(std::ptr::addr_of_mut!(data).cast()),
            Some(&mut size),
        )
    };
    match status {
        ERROR_FILE_NOT_FOUND => Ok(None),
        ERROR_SUCCESS if kind == REG_DWORD => Ok(Some(data)),
        ERROR_SUCCESS => Ok(None),
        other => Err(reg_error("RegQueryValueExW", value, other.0)),
    }
}

/// Writes a `REG_SZ`, creating the subkey when it does not exist.
pub fn write_string(root: Root, subkey: &str, value: &str, data: &str) -> io::Result<()> {
    let key = Key::open_or_create(root, subkey, KEY_SET_VALUE)?;
    let name = wide(value);
    let bytes: Vec<u8> = wide(data).iter().flat_map(|w| w.to_le_bytes()).collect();
    // SAFETY: `key` is open with KEY_SET_VALUE and `bytes` covers exactly the
    // NUL-terminated UTF-16 string.
    let status =
        unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&bytes)) };
    if status != ERROR_SUCCESS {
        return Err(reg_error("RegSetValueExW", value, status.0));
    }
    Ok(())
}

/// Deletes a value; missing counts as success.
pub fn delete_value(root: Root, subkey: &str, value: &str) -> io::Result<()> {
    let Some(key) = Key::open(root, subkey, KEY_SET_VALUE)? else {
        return Ok(());
    };
    let name = wide(value);
    // SAFETY: `key` is open with KEY_SET_VALUE; `name` is NUL-terminated.
    let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
    match status {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        other => Err(reg_error("RegDeleteValueW", value, other.0)),
    }
}

/// "Windows 11 Enterprise LTSC 24H2（内部版本 26100.1742）", for §18.
pub fn windows_version() -> String {
    const SUBKEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let product = read_string(Root::LocalMachine, SUBKEY, "ProductName")
        .ok()
        .flatten();
    let display = read_string(Root::LocalMachine, SUBKEY, "DisplayVersion")
        .ok()
        .flatten();
    let build = read_string(Root::LocalMachine, SUBKEY, "CurrentBuildNumber")
        .ok()
        .flatten();
    let ubr = read_dword(Root::LocalMachine, SUBKEY, "UBR").ok().flatten();
    let Some(product) = product else {
        return "未知".into();
    };
    let mut text = product;
    if let Some(display) = display {
        text.push(' ');
        text.push_str(&display);
    }
    if let Some(build) = build {
        text.push_str(&format!("（内部版本 {build}"));
        if let Some(ubr) = ubr {
            text.push_str(&format!(".{ubr}"));
        }
        text.push('）');
    }
    text
}

struct Key(HKEY);

impl Key {
    fn open(root: Root, subkey: &str, rights: REG_SAM_FLAGS) -> io::Result<Option<Self>> {
        let sub = wide(subkey);
        let mut key = HKEY::default();
        // SAFETY: `sub` is NUL-terminated and `key` is a live output slot.
        let status =
            unsafe { RegOpenKeyExW(root.hkey(), PCWSTR(sub.as_ptr()), None, rights, &mut key) };
        match status {
            ERROR_SUCCESS => Ok(Some(Self(key))),
            ERROR_FILE_NOT_FOUND => Ok(None),
            other => Err(reg_error("RegOpenKeyExW", subkey, other.0)),
        }
    }

    fn open_or_create(root: Root, subkey: &str, rights: REG_SAM_FLAGS) -> io::Result<Self> {
        let sub = wide(subkey);
        let mut key = HKEY::default();
        // SAFETY: `sub` is NUL-terminated and `key` is a live output slot.
        let status = unsafe {
            RegCreateKeyExW(
                root.hkey(),
                PCWSTR(sub.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                rights,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(reg_error("RegCreateKeyExW", subkey, status.0));
        }
        Ok(Self(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn reg_error(call: &str, name: &str, code: u32) -> io::Error {
    io::Error::other(format!("{call}({name}) 失败：{code}"))
}

fn from_utf16_bytes(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_owned()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
