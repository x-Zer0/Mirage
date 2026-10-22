//! UWP / AppContainer loopback exemption (iteration 2 §6): what
//! `CheckNetIsolation LoopbackExempt` does, via FirewallAPI. Listing works
//! as a normal user; writing needs elevation.
use std::io;
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        NetworkManagement::WindowsFirewall::{
            INET_FIREWALL_APP_CONTAINER, NetworkIsolationEnumAppContainers,
            NetworkIsolationFreeAppContainers, NetworkIsolationGetAppContainerConfig,
            NetworkIsolationSetAppContainerConfig,
        },
        Security::{
            Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW},
            PSID, SID_AND_ATTRIBUTES,
        },
        UI::Shell::SHLoadIndirectString,
    },
    core::{HSTRING, PWSTR},
};

/// AppContainer SIDs all start with this prefix.
pub const SID_PREFIX: &str = "S-1-15-2-";
const MAX_SID_LEN: usize = 184;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppContainer {
    pub sid: String,
    /// Package family name.
    pub name: String,
    pub display_name: String,
    pub exempt: bool,
}

/// Every AppContainer of the current user, sorted by display name.
pub fn list() -> io::Result<Vec<AppContainer>> {
    let exempt = exempt_sids()?;
    let mut count = 0u32;
    let mut first: *mut INET_FIREWALL_APP_CONTAINER = std::ptr::null_mut();
    // SAFETY: out-pointers are valid; the array is released below.
    let rc = unsafe { NetworkIsolationEnumAppContainers(0, &mut count, &mut first) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let mut apps = Vec::new();
    if !first.is_null() {
        // SAFETY: the API returned `count` contiguous entries at `first`.
        let entries = unsafe { std::slice::from_raw_parts(first, count as usize) };
        for entry in entries {
            // SAFETY: fields are valid for the lifetime of the array.
            let Some(sid) = (unsafe { sid_string(PSID(entry.appContainerSid.cast())) }) else {
                continue;
            };
            let name = unsafe { pwstr(entry.appContainerName) };
            let display = unsafe { pwstr(entry.displayName) };
            apps.push(AppContainer {
                exempt: exempt.contains(&sid),
                display_name: resolve_display(&display, &name),
                sid,
                name,
            });
        }
        // SAFETY: `first` came from NetworkIsolationEnumAppContainers.
        unsafe { NetworkIsolationFreeAppContainers(first) };
    }
    apps.sort_by_cached_key(|a| a.display_name.to_lowercase());
    apps.dedup_by(|a, b| a.sid == b.sid);
    Ok(apps)
}

/// SIDs currently exempt, including ones set by other tools.
pub fn exempt_sids() -> io::Result<Vec<String>> {
    let mut count = 0u32;
    let mut first: *mut SID_AND_ATTRIBUTES = std::ptr::null_mut();
    // SAFETY: out-pointers are valid; the array is released below.
    let rc = unsafe { NetworkIsolationGetAppContainerConfig(&mut count, &mut first) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    if first.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: the API returned `count` contiguous entries at `first`.
    let sids = unsafe { std::slice::from_raw_parts(first, count as usize) }
        .iter()
        .filter_map(|s| unsafe { sid_string(s.Sid) })
        .collect();
    // SAFETY: the array was allocated by the API for the caller to free.
    // The `Sid` pointers are not freed one by one: the API documentation
    // does not say they are separate allocations, and LocalFree on a pointer
    // into the array block would corrupt the heap. At worst this leaks a few
    // bytes per SID, once each time the dialog is opened.
    unsafe { LocalFree(Some(HLOCAL(first.cast()))) };
    Ok(sids)
}

/// Replaces the whole exemption list (needs elevation). Callers pass the
/// complete target list so exemptions made by other tools survive.
pub fn set_exempt(sids: &[String]) -> io::Result<()> {
    let mut converted: Vec<PSID> = Vec::with_capacity(sids.len());
    let result = (|| {
        for sid in sids {
            if !valid_sid(sid) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "SID 不合法"));
            }
            let mut psid = PSID::default();
            // SAFETY: `psid` receives a LocalAlloc'ed SID, freed below.
            unsafe { ConvertStringSidToSidW(&HSTRING::from(sid.as_str()), &mut psid) }
                .map_err(io::Error::other)?;
            converted.push(psid);
        }
        let entries: Vec<SID_AND_ATTRIBUTES> = converted
            .iter()
            .map(|sid| SID_AND_ATTRIBUTES {
                Sid: *sid,
                Attributes: 0,
            })
            .collect();
        // SAFETY: every entry points at a SID alive until the frees below.
        let rc = unsafe { NetworkIsolationSetAppContainerConfig(&entries) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    })();
    for sid in converted {
        // SAFETY: allocated by ConvertStringSidToSidW.
        unsafe { LocalFree(Some(HLOCAL(sid.0))) };
    }
    result
}

/// `S-1-15-2-` followed by dash-separated decimal sub-authorities.
pub fn valid_sid(sid: &str) -> bool {
    sid.len() <= MAX_SID_LEN
        && sid.strip_prefix(SID_PREFIX).is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .split('-')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// `@{Package?ms-resource://…}` display names are resource references.
fn resolve_display(display: &str, name: &str) -> String {
    if display.starts_with('@') {
        let mut buf = [0u16; 512];
        // SAFETY: `buf` is a valid output buffer for its length.
        if unsafe { SHLoadIndirectString(&HSTRING::from(display), &mut buf, None) }.is_ok() {
            let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            let text = String::from_utf16_lossy(&buf[..len]);
            if !text.trim().is_empty() {
                return text;
            }
        }
        return name.to_owned();
    }
    if display.trim().is_empty() {
        name.to_owned()
    } else {
        display.to_owned()
    }
}

/// # Safety
/// `sid` must be null or point at a valid SID.
unsafe fn sid_string(sid: PSID) -> Option<String> {
    if sid.0.is_null() {
        return None;
    }
    let mut text = PWSTR::null();
    // SAFETY: `text` receives a LocalAlloc'ed string, freed below.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.ok()?;
    let value = unsafe { text.to_string() }.ok();
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    value
}

/// # Safety
/// `text` must be null or a valid NUL-terminated string.
unsafe fn pwstr(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    unsafe { text.to_string() }.unwrap_or_default()
}
