//! Read-only import of the original client's `config.yaml` (ZIYOUMAO.md §9).
//! The file is plain YAML, plain Base64, or Base64 over a mapped alphabet;
//! each decoding is tried at most once. The result still has to pass the
//! generic pipeline (`detect` + `mihomo -t`).

use super::STAGE;
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::subscriptions::import::MAX_CONFIG_BYTES;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_yaml_ng::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

const MIHOMO_KEYS: [&str; 4] = ["proxies", "proxy-providers", "proxy-groups", "rules"];

/// `%APPDATA%\ziyoumao\ziyoumao\config.yaml`.
pub fn default_path() -> AppResult<PathBuf> {
    crate::platform::windows::known_folder::roaming_app_data()
        .map(|d| d.join("ziyoumao").join("ziyoumao").join("config.yaml"))
        .map_err(|e| AppError::storage(STAGE, e))
}

/// Reads (never modifies) the file and returns plain YAML bytes.
pub fn read(path: &Path) -> AppResult<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::new(
                AppErrorKind::Storage,
                STAGE,
                "没有找到原自由猫客户端的配置文件",
            )
        } else {
            AppError::storage(STAGE, e)
        }
    })?;
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
    decode(&raw).map(String::into_bytes)
}

fn decode(raw: &[u8]) -> AppResult<String> {
    let text = std::str::from_utf8(raw).map_err(|_| unsupported("配置文件不是文本"))?;
    let text = text.trim_start_matches('\u{feff}').trim();
    if looks_like_mihomo_yaml(text) {
        return Ok(text.to_owned());
    }
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(s) = b64_utf8(&compact)
        && looks_like_mihomo_yaml(&s)
    {
        return Ok(s);
    }
    let mapped: String = compact.chars().map(map_char).collect();
    match b64_utf8(&mapped) {
        Some(s) if looks_like_mihomo_yaml(&s) => Ok(s),
        // Includes the unsampled DecodeAESBase64 / DecryptConfig variants.
        _ => Err(unsupported(
            "无法解码原客户端配置（可能是不支持的加密格式）",
        )),
    }
}

/// Mapped → standard alphabet: A–Z, a–z and 0–9 reversed, `+` ↔ `/`.
fn map_char(c: char) -> char {
    let rev = |c: char, lo: u8, hi: u8| char::from(hi - (c as u8 - lo));
    match c {
        'A'..='Z' => rev(c, b'A', b'Z'),
        'a'..='z' => rev(c, b'a', b'z'),
        '0'..='9' => rev(c, b'0', b'9'),
        '+' => '/',
        '/' => '+',
        other => other,
    }
}

fn b64_utf8(text: &str) -> Option<String> {
    String::from_utf8(STANDARD.decode(text).ok()?).ok()
}

fn looks_like_mihomo_yaml(text: &str) -> bool {
    matches!(
        serde_yaml_ng::from_str::<Value>(text),
        Ok(Value::Mapping(map)) if MIHOMO_KEYS.iter().any(|k| map.contains_key(*k))
    )
}

fn unsupported(detail: &str) -> AppError {
    AppError::new(AppErrorKind::UnsupportedFormat, STAGE, detail)
}
