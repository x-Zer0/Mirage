//! Conventional subscription response headers (DESIGN.md §15). Display only;
//! malformed values are ignored, never fatal.

use crate::domain::model::UsageInfo;
use percent_encoding::percent_decode_str;
use reqwest::header::HeaderMap;

#[derive(Debug, Clone, Default)]
pub struct SubscriptionMeta {
    pub usage: Option<UsageInfo>,
    /// `profile-update-interval`, hours.
    pub update_interval: Option<u32>,
    /// Default profile name from `content-disposition`.
    pub filename: Option<String>,
}

pub fn parse(headers: &HeaderMap) -> SubscriptionMeta {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    SubscriptionMeta {
        usage: text("subscription-userinfo").and_then(parse_userinfo),
        update_interval: text("profile-update-interval")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|h| h.is_finite() && *h > 0.0)
            .map(|h| h.ceil().min(f64::from(24 * 30)) as u32),
        filename: text("content-disposition").and_then(parse_filename),
    }
}

/// `upload=1; download=2; total=3; expire=4` in any order.
fn parse_userinfo(value: &str) -> Option<UsageInfo> {
    let mut usage = UsageInfo::default();
    let mut seen = false;
    for part in value.split(';') {
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        // Some services send floats ("1.5e10"); round them down.
        let Some(n) = val.trim().parse::<f64>().ok().filter(|n| *n >= 0.0) else {
            continue;
        };
        let n = n as u64;
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => usage.upload = n,
            "download" => usage.download = n,
            "total" => usage.total = n,
            "expire" => usage.expire = (n > 0).then_some(n as i64),
            _ => continue,
        }
        seen = true;
    }
    seen.then_some(usage)
}

/// Prefers RFC 5987 `filename*=UTF-8''…`, falls back to `filename="…"`.
fn parse_filename(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        if let Some(rest) = part.strip_prefix("filename*=") {
            let encoded = rest.rsplit("''").next().unwrap_or(rest);
            if let Ok(name) = percent_decode_str(encoded.trim_matches('"')).decode_utf8() {
                return clean_name(&name);
            }
        } else if let Some(rest) = part.strip_prefix("filename=") {
            plain = clean_name(rest.trim_matches('"'));
        }
    }
    plain
}

fn clean_name(name: &str) -> Option<String> {
    let stem = name
        .trim()
        .trim_end_matches(".yaml")
        .trim_end_matches(".yml")
        .trim();
    let stem: String = stem.chars().filter(|c| !c.is_control()).take(64).collect();
    (!stem.is_empty()).then_some(stem)
}
