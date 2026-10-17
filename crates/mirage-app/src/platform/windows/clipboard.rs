//! Plain-text clipboard write. Used only for "复制订阅链接", the one place a
//! full subscription URL leaves the SecretStore on user request (§17.3).

use std::io;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

/// `CF_UNICODETEXT`; spelled out to avoid the OLE feature for one constant.
const CF_UNICODETEXT: u32 = 13;

pub fn set_text(text: &str) -> io::Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * size_of::<u16>();
    // SAFETY: the buffer is allocated, filled and handed to the clipboard as
    // documented; on a failed SetClipboardData we still own and free it.
    unsafe {
        OpenClipboard(None).map_err(io::Error::other)?;
        let result = (|| {
            EmptyClipboard()?;
            let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
            let ptr = GlobalLock(mem).cast::<u16>();
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err(windows::core::Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e);
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result.map_err(io::Error::other)
    }
}
