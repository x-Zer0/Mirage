//! Conflicts with the original ziyoumao client (DESIGN.md §11.8, ZIYOUMAO.md
//! appendix). Detection is read-only: the user is told to exit the other
//! client, never having it ended for them.

use crate::platform::windows::{process, sysproxy};
use std::collections::BTreeSet;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    GAA_FLAG_SKIP_UNICAST, GET_ADAPTERS_ADDRESSES_FLAGS, GetAdaptersAddresses, GetExtendedTcpTable,
    IP_ADAPTER_ADDRESSES_LH, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    TCP_TABLE_OWNER_PID_LISTENER,
};
use windows::Win32::Networking::WinSock::AF_INET;

/// Image names of the original client (ZIYOUMAO.md appendix).
const LEGACY_PROCESSES: [&str; 2] = ["ziyoumao.exe", "ziyoumaoCore.exe"];
/// A wintun adapter installed by a Mihomo-based client.
const TUN_MARKERS: [&str; 3] = ["mihomo", "meta", "wintun"];

/// One detected conflict, already phrased for the user.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Conflict {
    pub summary: String,
    pub detail: String,
}

impl Conflict {
    pub fn text(&self) -> String {
        format!("{}（{}）", self.summary, self.detail)
    }
}

/// Everything found right now; empty when the machine is clear.
///
/// `mixed_port` filters the system-proxy rule: our own port is not a conflict.
pub fn detect(mixed_port: u16) -> Vec<Conflict> {
    let mut found = legacy_processes();
    found.extend(tun_adapters());
    found.extend(foreign_system_proxy(mixed_port));
    found
}

/// The original client's UI or core process (§11.8).
fn legacy_processes() -> Vec<Conflict> {
    let running: BTreeSet<String> = process::image_names()
        .into_iter()
        .map(|name| name.to_lowercase())
        .collect();
    LEGACY_PROCESSES
        .into_iter()
        .filter(|name| running.contains(&name.to_lowercase()))
        .map(|name| Conflict {
            summary: "原自由猫客户端正在运行".into(),
            detail: format!("检测到进程 {name}，请先完全退出原客户端"),
        })
        .collect()
}

/// wintun adapters, which usually belong to the original client's TUN (§11.8).
fn tun_adapters() -> Vec<Conflict> {
    adapters()
        .into_iter()
        .filter(|(name, description)| {
            let name = name.to_lowercase();
            let description = description.to_lowercase();
            TUN_MARKERS
                .iter()
                .any(|m| name.contains(m) || description.contains(m))
        })
        .map(|(name, description)| Conflict {
            summary: "存在 Mihomo 类虚拟网卡".into(),
            detail: format!(
                "{name}{}",
                if description.is_empty() {
                    String::new()
                } else {
                    format!("（{description}）")
                }
            ),
        })
        .collect()
}

/// A system proxy pointing at a local port that is not ours (§11.8).
fn foreign_system_proxy(mixed_port: u16) -> Vec<Conflict> {
    let Ok(current) = sysproxy::read() else {
        return Vec::new();
    };
    if !current.proxy_enabled() {
        return Vec::new();
    }
    let Some(server) = current.server.as_deref() else {
        return Vec::new();
    };
    let ours = format!("127.0.0.1:{mixed_port}");
    // A multi-entry list is normal; only a purely foreign local route matters.
    let locals: Vec<&str> = server.split(';').filter(|p| is_local(p)).collect();
    if locals.is_empty() || locals.iter().any(|p| *p == ours) {
        return Vec::new();
    }
    vec![Conflict {
        summary: "系统代理指向其他本地端口".into(),
        detail: format!("当前为 {}，原客户端可能仍在接管", locals.join("、")),
    }]
}

/// `host:port` or `scheme=host:port` on the loopback address.
fn is_local(entry: &str) -> bool {
    let entry = entry.rsplit('=').next().unwrap_or(entry).trim();
    entry.starts_with("127.") || entry.starts_with("localhost:") || entry.starts_with("[::1]")
}

/// `(friendly name, description)` of every adapter, TUN kinds included.
fn adapters() -> Vec<(String, String)> {
    // The first call sizes the buffer; ERROR_BUFFER_OVERFLOW is the expected
    // answer, not a failure.
    let mut size = 16 * 1024u32;
    let flags = GAA_FLAG_SKIP_UNICAST
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    // M20: 用 Vec<u64> 确保正确对齐
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: `buf` is a live allocation of `size` bytes; the API writes at most
    // that much and reports the required size when it does not fit.
    let status = unsafe {
        GetAdaptersAddresses(
            AF_INET.0 as u32,
            GET_ADAPTERS_ADDRESSES_FLAGS(flags.0 | GAA_FLAG_SKIP_DNS_SERVER.0),
            None,
            Some(buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
            &mut size,
        )
    };
    if status == ERROR_BUFFER_OVERFLOW.0 {
        buf.resize((size as usize).div_ceil(8), 0);
        // SAFETY: the buffer now matches the size the API asked for.
        let retry = unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                GET_ADAPTERS_ADDRESSES_FLAGS(flags.0),
                None,
                Some(buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
                &mut size,
            )
        };
        if retry != ERROR_SUCCESS.0 {
            tracing::debug!(status = retry, "GetAdaptersAddresses retry failed");
            return Vec::new();
        }
    } else if status != ERROR_SUCCESS.0 {
        tracing::debug!(status, "GetAdaptersAddresses failed");
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut node = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    // SAFETY: the API filled the buffer with a NUL-terminated linked list; each
    // node stays inside the allocation and the strings are NUL-terminated.
    unsafe {
        while let Some(adapter) = node.as_ref() {
            out.push((pwstr(adapter.FriendlyName), pwstr(adapter.Description)));
            node = adapter.Next;
        }
    }
    out
}

/// Local address of the process listening on `port`, for a `PortInUse` hint.
pub fn port_owner(port: u16) -> Option<String> {
    let mut size = 16 * 1024u32;
    // M20: 用 Vec<u64> 确保正确对齐
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: sizing call; the table is written into `buf` when it fits.
    let status = unsafe {
        GetExtendedTcpTable(
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status == ERROR_BUFFER_OVERFLOW.0 || status == ERROR_INSUFFICIENT_BUFFER.0 {
        buf.resize((size as usize).div_ceil(8), 0);
    }
    // SAFETY: `buf` now matches the reported size (or the call fit the first time).
    let status = unsafe {
        GetExtendedTcpTable(
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status != ERROR_SUCCESS.0 {
        return None;
    }
    // SAFETY: the table header and its `dwNumEntries` rows are inside `buf`.
    unsafe {
        let table = buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
        let count = (*table).dwNumEntries as usize;
        let rows = std::slice::from_raw_parts(
            std::ptr::addr_of!((*table).table).cast::<MIB_TCPROW_OWNER_PID>(),
            count,
        );
        for row in rows {
            // The port is stored in the low two bytes, network byte order.
            let local = u16::from_be((row.dwLocalPort & 0xffff) as u16);
            if local == port
                && let Some(name) = process::image_name(row.dwOwningPid)
            {
                return Some(name);
            }
        }
    }
    None
}

/// Copies a NUL-terminated wide string; empty when the pointer is null.
unsafe fn pwstr(p: windows::core::PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees a NUL-terminated buffer owned by the
    // adapter list we are walking.
    unsafe { p.to_string().unwrap_or_default() }
}
