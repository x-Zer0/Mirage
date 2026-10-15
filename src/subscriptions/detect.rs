//! Content-based format detection (DESIGN.md §8 step 3). Never trusts file
//! extension or Content-Type; decodes plain Base64 at most once.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::ConfigFormat;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_yaml_ng::Value;

const STAGE: &str = "subscriptions.detect";
const MIHOMO_KEYS: [&str; 4] = ["proxies", "proxy-providers", "proxy-groups", "rules"];

/// Detected configuration text, already decoded to UTF-8.
pub struct Detected {
    pub format: ConfigFormat,
    pub text: String,
}

pub fn detect(raw: &[u8]) -> AppResult<Detected> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| err(AppErrorKind::NotAConfig, "内容不是 UTF-8 文本"))?;
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Err(err(AppErrorKind::NotAConfig, "内容为空"));
    }
    if looks_like_html(text) {
        return Err(err(AppErrorKind::NotAConfig, "收到的是网页而不是配置"));
    }
    if text.starts_with('{') && serde_json::from_str::<serde_json::Map<_, _>>(text).is_ok() {
        return Err(err(
            AppErrorKind::UnsupportedFormat,
            "sing-box 配置暂不支持",
        ));
    }
    if is_mihomo_yaml(text) {
        return Ok(Detected {
            format: ConfigFormat::MihomoYaml,
            text: text.to_owned(),
        });
    }
    if let Some(decoded) = decode_base64_once(text) {
        let decoded = decoded.trim();
        if is_mihomo_yaml(decoded) {
            return Ok(Detected {
                format: ConfigFormat::MihomoYaml,
                text: decoded.to_owned(),
            });
        }
        if is_link_list(decoded) {
            return Err(err(
                AppErrorKind::UnsupportedFormat,
                "节点链接列表订阅暂不支持",
            ));
        }
    }
    if is_link_list(text) {
        return Err(err(
            AppErrorKind::UnsupportedFormat,
            "节点链接列表订阅暂不支持",
        ));
    }
    Err(err(
        AppErrorKind::UnsupportedFormat,
        "无法识别为 Mihomo 配置",
    ))
}

fn looks_like_html(text: &str) -> bool {
    let head: String = text
        .chars()
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    ["<!doctype", "<html", "<head"]
        .iter()
        .any(|p| head.starts_with(p))
}

fn is_mihomo_yaml(text: &str) -> bool {
    match serde_yaml_ng::from_str::<Value>(text) {
        Ok(Value::Mapping(map)) => MIHOMO_KEYS.iter().any(|k| map.contains_key(*k)),
        _ => false,
    }
}

fn decode_base64_once(text: &str) -> Option<String> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD.decode(compact).ok()?;
    String::from_utf8(bytes).ok()
}

fn is_link_list(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next();
    first.is_some_and(is_link) && lines.all(is_link)
}

fn is_link(line: &str) -> bool {
    line.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

fn err(kind: AppErrorKind, detail: &str) -> AppError {
    AppError::new(kind, STAGE, detail)
}
