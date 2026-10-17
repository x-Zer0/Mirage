//! WinINet per-connection proxy settings of the default LAN connection
//! (DESIGN.md §11.2). The registry is never written directly, so running
//! programs receive the change notification.

use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::io;
use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows::Win32::Networking::WinInet::{
    INTERNET_OPTION_PER_CONNECTION_OPTION, INTERNET_OPTION_REFRESH,
    INTERNET_OPTION_SETTINGS_CHANGED, INTERNET_PER_CONN, INTERNET_PER_CONN_AUTOCONFIG_URL,
    INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_OPTION_LISTW, INTERNET_PER_CONN_OPTIONW,
    INTERNET_PER_CONN_PROXY_BYPASS, INTERNET_PER_CONN_PROXY_SERVER, InternetQueryOptionW,
    InternetSetOptionW, PROXY_TYPE_AUTO_PROXY_URL, PROXY_TYPE_DIRECT, PROXY_TYPE_PROXY,
};
use windows::core::PWSTR;

pub const FLAG_DIRECT: u32 = PROXY_TYPE_DIRECT;
pub const FLAG_PROXY: u32 = PROXY_TYPE_PROXY;
const FLAG_PAC: u32 = PROXY_TYPE_AUTO_PROXY_URL;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySettings {
    pub flags: u32,
    pub server: Option<String>,
    pub bypass: Option<String>,
    pub autoconfig_url: Option<String>,
}

impl ProxySettings {
    pub fn proxy_enabled(&self) -> bool {
        self.flags & FLAG_PROXY != 0
    }

    /// Equality of the parts that decide where traffic goes. Auto-detect is
    /// ignored because Windows may toggle it on its own.
    pub fn same_route(&self, other: &Self) -> bool {
        let mask = FLAG_PROXY | FLAG_PAC;
        if self.flags & mask != other.flags & mask {
            return false;
        }
        let proxy_same =
            !self.proxy_enabled() || (self.server == other.server && self.bypass == other.bypass);
        let pac_same = self.flags & FLAG_PAC == 0 || self.autoconfig_url == other.autoconfig_url;
        proxy_same && pac_same
    }
}

const FIELDS: [INTERNET_PER_CONN; 4] = [
    INTERNET_PER_CONN_FLAGS,
    INTERNET_PER_CONN_PROXY_SERVER,
    INTERNET_PER_CONN_PROXY_BYPASS,
    INTERNET_PER_CONN_AUTOCONFIG_URL,
];

pub fn read() -> io::Result<ProxySettings> {
    let mut options = FIELDS.map(|field| INTERNET_PER_CONN_OPTIONW {
        dwOption: field,
        ..Default::default()
    });
    let mut list = option_list(&mut options);
    let mut len = size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32;
    // SAFETY: `list` points at `options`, both live for the call; WinINet
    // allocates the returned strings with GlobalAlloc and `take_string` frees them.
    unsafe {
        InternetQueryOptionW(
            None,
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            Some(&mut list as *mut _ as *mut c_void),
            &mut len,
        )
        .map_err(io::Error::other)?;
        Ok(ProxySettings {
            flags: options[0].Value.dwValue,
            server: take_string(options[1].Value.pszValue),
            bypass: take_string(options[2].Value.pszValue),
            autoconfig_url: take_string(options[3].Value.pszValue),
        })
    }
}

pub fn write(settings: &ProxySettings) -> io::Result<()> {
    let mut server = wide(settings.server.as_deref());
    let mut bypass = wide(settings.bypass.as_deref());
    let mut pac = wide(settings.autoconfig_url.as_deref());
    let mut options = FIELDS.map(|field| INTERNET_PER_CONN_OPTIONW {
        dwOption: field,
        ..Default::default()
    });
    options[0].Value.dwValue = settings.flags;
    options[1].Value.pszValue = PWSTR(server.as_mut_ptr());
    options[2].Value.pszValue = PWSTR(bypass.as_mut_ptr());
    options[3].Value.pszValue = PWSTR(pac.as_mut_ptr());
    let list = option_list(&mut options);
    // SAFETY: every pointer in `list` refers to a NUL-terminated buffer or the
    // options array above, all alive until the calls return.
    unsafe {
        InternetSetOptionW(
            None,
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            Some(&list as *const _ as *const c_void),
            size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        )
        .map_err(io::Error::other)?;
        InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0)
            .map_err(io::Error::other)?;
        InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0).map_err(io::Error::other)?;
    }
    Ok(())
}

/// Default LAN connection only (`pszConnection = NULL`); dial-up/VPN entries are untouched.
fn option_list(options: &mut [INTERNET_PER_CONN_OPTIONW; 4]) -> INTERNET_PER_CONN_OPTION_LISTW {
    INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        pszConnection: PWSTR::null(),
        dwOptionCount: options.len() as u32,
        dwOptionError: 0,
        pOptions: options.as_mut_ptr(),
    }
}

/// Copies and frees a WinINet-allocated string; empty strings become `None`.
unsafe fn take_string(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: WinINet returned a NUL-terminated GlobalAlloc'd buffer that we own.
    let text = unsafe { p.to_string() }.ok();
    let _ = unsafe { GlobalFree(Some(HGLOBAL(p.0.cast()))) };
    text.filter(|s| !s.is_empty())
}

fn wide(s: Option<&str>) -> Vec<u16> {
    s.unwrap_or_default()
        .encode_utf16()
        .chain(Some(0))
        .collect()
}
