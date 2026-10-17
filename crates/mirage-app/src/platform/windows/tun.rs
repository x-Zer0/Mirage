//! Read-only readiness checks for the core's TUN adapter and IPv4 routes.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceAliasToLuid, GetBestInterface, GetIfEntry2, MIB_IF_ROW2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::core::PCWSTR;

/// `auto-route` must select this live adapter for both halves of the internet.
pub fn is_ready(device: &str) -> bool {
    let alias: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
    let mut adapter = MIB_IF_ROW2::default();
    // SAFETY: `alias` is NUL-terminated and both API outputs are live stack values.
    unsafe {
        if ConvertInterfaceAliasToLuid(PCWSTR(alias.as_ptr()), &mut adapter.InterfaceLuid)
            != ERROR_SUCCESS
            || GetIfEntry2(&mut adapter) != ERROR_SUCCESS
            || adapter.OperStatus != IfOperStatusUp
        {
            return false;
        }
    }
    [[1, 1, 1, 1], [208, 67, 222, 222]]
        .into_iter()
        .all(|address| route_uses(address, adapter.InterfaceIndex))
}

fn route_uses(address: [u8; 4], adapter_index: u32) -> bool {
    let mut route_index = 0;
    // SAFETY: the address is in network byte order; the output pointer is valid.
    // This only reads the route table and sends no traffic to the destination.
    unsafe {
        GetBestInterface(u32::from_ne_bytes(address), &mut route_index) == ERROR_SUCCESS.0
            && route_index == adapter_index
    }
}
