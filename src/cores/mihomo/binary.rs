//! Core binary verification and core-home seeding (DESIGN.md §10.1, §10.2).

use super::process::NO_WINDOW;
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::storage::layout::Layout;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const STAGE: &str = "mihomo.binary";
pub const EXPECTED_SHA256: &str =
    "beb9878924d7bd38c67176b441ab5e668cc378bfe751c8dc8fef3eb5aaf566d5";
pub const EXPECTED_VERSION: &str = "v1.19.32";
const GEODATA: [&str; 4] = ["GEOIP.metadb", "GEOSITE.dat", "GEOIP.dat", "ASN.mmdb"];

#[derive(Debug, Clone)]
pub struct CoreBinary {
    pub exe: PathBuf,
    /// e.g. "Mihomo Meta v1.19.32 windows amd64 ..."
    pub version_line: String,
}

impl CoreBinary {
    pub fn short_version(&self) -> String {
        self.version_line
            .split_whitespace()
            .find(|w| w.starts_with('v') && w[1..].starts_with(|c: char| c.is_ascii_digit()))
            .map_or_else(|| "unknown".to_owned(), |v| format!("mihomo {v}"))
    }
}

pub async fn verify(layout: &Layout) -> AppResult<CoreBinary> {
    let exe = layout.bundled_core.join("mihomo.exe");
    let hashed = exe.clone();
    let digest = tokio::task::spawn_blocking(move || hash_file(&hashed))
        .await
        .map_err(|e| AppError::internal(STAGE, e))??;
    if digest != EXPECTED_SHA256 {
        return Err(AppError::new(
            AppErrorKind::CoreBinaryInvalid,
            STAGE,
            format!("mihomo.exe 的 SHA256 与固定版本 {EXPECTED_VERSION} 不一致"),
        ));
    }
    let version_line = run_version(&exe).await?;
    if !version_line.contains(EXPECTED_VERSION) {
        return Err(AppError::new(
            AppErrorKind::CoreBinaryInvalid,
            STAGE,
            format!("内核版本输出异常：{version_line}"),
        ));
    }
    Ok(CoreBinary { exe, version_line })
}

/// Copies bundled geodata into core-home when missing, so `-t` never needs
/// to download it on first run (§10.2).
pub fn seed_core_home(layout: &Layout) -> AppResult<()> {
    for name in GEODATA {
        let target = layout.core_home.join(name);
        if target.is_file() {
            continue;
        }
        let source = layout.bundled_core.join(name);
        if !source.is_file() {
            tracing::warn!(file = name, "bundled geodata missing");
            continue;
        }
        std::fs::copy(&source, &target).map_err(|e| AppError::storage(STAGE, e))?;
    }
    Ok(())
}

fn hash_file(path: &Path) -> AppResult<String> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        AppError::new(
            AppErrorKind::CoreBinaryInvalid,
            STAGE,
            format!("找不到内核 {}：{e}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| AppError::storage(STAGE, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

async fn run_version(exe: &Path) -> AppResult<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(exe)
            .arg("-v")
            .creation_flags(NO_WINDOW)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| AppError::new(AppErrorKind::Timeout, STAGE, "mihomo -v 超时"))?
    .map_err(|e| AppError::new(AppErrorKind::CoreBinaryInvalid, STAGE, e.to_string()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().next().unwrap_or_default().trim().to_owned())
}
