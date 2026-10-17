//! Local file import (DESIGN.md §5 LocalFile source).

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use std::io::Read;
use std::path::Path;

const STAGE: &str = "subscriptions.import";
/// Same ceiling as subscription downloads (DESIGN.md §12.1).
pub const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

pub struct Imported {
    pub name: String,
    pub raw: Vec<u8>,
}

/// Reads the file with a size limit. The original file is never modified.
pub fn read_local(path: &Path) -> AppResult<Imported> {
    let file = std::fs::File::open(path).map_err(|e| AppError::storage(STAGE, e))?;
    let mut raw = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| AppError::storage(STAGE, e))?;
    if raw.len() as u64 > MAX_CONFIG_BYTES {
        return Err(AppError::new(
            AppErrorKind::ResponseTooLarge,
            STAGE,
            "配置文件超过 16 MiB",
        ));
    }
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("本地配置")
        .to_owned();
    Ok(Imported { name, raw })
}
