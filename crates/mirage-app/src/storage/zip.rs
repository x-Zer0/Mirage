//! Minimal ZIP writer for the diagnostics bundle (DESIGN.md §18).
//!
//! Only the "stored" method is used: the bundle holds text that compresses
//! poorly per entry and the archive is written once, so pulling in a full
//! deflate dependency (§19) is not worth it. CRC-32 is computed here.

use std::io;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const EOCD: u32 = 0x0605_4b50;

/// Entries are held in memory until `finish`, so the caller can bound the
/// total size before anything is written.
#[derive(Default)]
pub struct ZipWriter {
    entries: Vec<Entry>,
    /// Unix seconds used for every DOS timestamp field.
    dos_time: u16,
    dos_date: u16,
}

struct Entry {
    name: String,
    data: Vec<u8>,
    crc: u32,
}

impl ZipWriter {
    /// `stamp` is the local time recorded for every entry (the bundle is a
    /// point-in-time snapshot, so one timestamp is correct).
    pub fn new(stamp: chrono::DateTime<chrono::Local>) -> Self {
        use chrono::{Datelike, Timelike};
        let year = stamp.year().clamp(1980, 2107) as u16;
        Self {
            entries: Vec::new(),
            dos_time: ((stamp.hour() as u16) << 11)
                | ((stamp.minute() as u16) << 5)
                | (stamp.second() as u16 / 2),
            dos_date: ((year - 1980) << 9) | ((stamp.month() as u16) << 5) | stamp.day() as u16,
        }
    }

    /// Adds a text entry; `name` is the in-archive path with `/` separators.
    pub fn add_text(&mut self, name: &str, text: &str) {
        // M24: 拒绝路径遍历
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            tracing::warn!(name, "zip entry name rejected (path traversal)");
            return;
        }
        let bytes = text.as_bytes().to_vec();
        let crc = crc32(&bytes);
        self.entries.push(Entry {
            name: name.replace('\\', "/"),
            data: bytes,
            crc,
        });
    }

    /// Uncompressed size of everything added so far.
    pub fn len(&self) -> usize {
        self.entries.iter().map(|e| e.data.len()).sum()
    }

    /// Writes the archive: local headers, then the central directory, then EOCD.
    pub fn finish(self) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(self.len() + 1024);
        let mut offsets = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            offsets.push(out.len() as u32);
            let name = entry.name.as_bytes();
            write_u32(&mut out, LOCAL_HEADER);
            write_u16(&mut out, 20); // version needed
            write_u16(&mut out, 0x0800); // UTF-8 file names
            write_u16(&mut out, 0); // stored
            write_u16(&mut out, self.dos_time);
            write_u16(&mut out, self.dos_date);
            write_u32(&mut out, entry.crc);
            write_u32(&mut out, entry.data.len() as u32);
            write_u32(&mut out, entry.data.len() as u32);
            write_u16(&mut out, name.len() as u16);
            write_u16(&mut out, 0); // extra field length
            out.extend_from_slice(name);
            out.extend_from_slice(&entry.data);
        }
        let central_start = out.len() as u32;
        for (entry, offset) in self.entries.iter().zip(&offsets) {
            let name = entry.name.as_bytes();
            write_u32(&mut out, CENTRAL_HEADER);
            write_u16(&mut out, 20); // version made by
            write_u16(&mut out, 20); // version needed
            write_u16(&mut out, 0x0800);
            write_u16(&mut out, 0);
            write_u16(&mut out, self.dos_time);
            write_u16(&mut out, self.dos_date);
            write_u32(&mut out, entry.crc);
            write_u32(&mut out, entry.data.len() as u32);
            write_u32(&mut out, entry.data.len() as u32);
            write_u16(&mut out, name.len() as u16);
            write_u16(&mut out, 0); // extra
            write_u16(&mut out, 0); // comment
            write_u16(&mut out, 0); // disk number
            write_u16(&mut out, 0); // internal attributes
            write_u32(&mut out, 0); // external attributes
            write_u32(&mut out, *offset);
            out.extend_from_slice(name);
        }
        let central_size = out.len() as u32 - central_start;
        let count = self.entries.len() as u16;
        write_u32(&mut out, EOCD);
        write_u16(&mut out, 0);
        write_u16(&mut out, 0);
        write_u16(&mut out, count);
        write_u16(&mut out, count);
        write_u32(&mut out, central_size);
        write_u32(&mut out, central_start);
        write_u16(&mut out, 0); // archive comment
        Ok(out)
    }
}

fn write_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Standard CRC-32 (IEEE 802.3), built once per call site.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Writes the archive to `path` atomically, so a failed export never leaves a
/// half-written bundle behind.
pub fn write_file(path: &std::path::Path, zip: ZipWriter) -> io::Result<()> {
    let bytes = zip.finish()?;
    super::atomic::write(path, &bytes)
}
