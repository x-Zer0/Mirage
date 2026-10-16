//! Request construction, response envelope and error mapping for the
//! ziyoumao account API (ZIYOUMAO.md §5, §8).
//!
//! Every call goes to `<API base>` = `<host>/api/v1/` through a dedicated
//! `PROVIDER_API` client (no redirects, 1 MiB, 20 s). The session token is
//! attached per request, never as a default header, and requests never carry
//! cookies.

use super::STAGE;
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::EgressPolicy;
use crate::net::{self, EgressRoute};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// GET retries on transport errors and 5xx: 1 s, then 3 s (§8).
const GET_BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];
/// 429 is waited out once, at most this long.
const MAX_RETRY_AFTER: u64 = 60;
const DEFAULT_RETRY_AFTER: u64 = 5;
const MESSAGE_LIMIT: usize = 200;

/// Where and how requests leave the machine.
#[derive(Debug, Clone, Copy)]
pub struct Egress {
    pub policy: EgressPolicy,
    pub core_port: Option<u16>,
}

/// One HTTP response, body already read with the size limit.
pub struct Raw {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// V2Board-style envelope (§5.1). `errors` is only kept for field names.
#[derive(Deserialize)]
struct Envelope {
    data: Option<serde_json::Value>,
    message: Option<String>,
}

/// Builds `<base><path>`; `path` is relative, e.g. `user/info`.
pub fn endpoint(base: &Url, path: &str) -> AppResult<Url> {
    base.join(path)
        .map_err(|_| AppError::internal(STAGE, "无效的接口路径"))
}

/// Sends once through `route`. `session` becomes `Authorization: <token>`
/// (raw value, no `Bearer`, §6.2).
pub async fn send(
    method: Method,
    url: &Url,
    session: Option<&str>,
    body: Option<bytes::Bytes>,
    route: EgressRoute,
) -> AppResult<Raw> {
    net::check_domain_url(url, false)?;
    let client = net::client(&net::PROVIDER_API, route)?;
    let mut req = client
        .request(method, url.clone())
        .header(ACCEPT, "application/json");
    if let Some(token) = session {
        let mut value =
            HeaderValue::from_str(token).map_err(|_| AppError::internal(STAGE, "会话令牌无效"))?;
        value.set_sensitive(true);
        req = req.header(AUTHORIZATION, value);
    }
    if let Some(body) = body {
        req = req.header(CONTENT_TYPE, "application/json").body(body);
    }
    let resp = req.send().await.map_err(net::map_error)?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = net::read_bounded(resp, net::PROVIDER_API.max_body).await?;
    Ok(Raw {
        status,
        headers,
        body,
    })
}

/// GET with the egress policy and the §8 retry rules; returns `data`.
pub async fn get_data(
    url: &Url,
    session: Option<&str>,
    egress: Egress,
) -> AppResult<serde_json::Value> {
    let mut waited_rate_limit = false;
    let mut backoff = GET_BACKOFF.iter();
    loop {
        let result = net::with_egress(egress.policy, egress.core_port, |route| {
            send(Method::GET, url, session, None, route)
        })
        .await
        .and_then(|raw| data_of(&raw));
        let err = match result {
            Ok(data) => return Ok(data),
            Err(e) => e,
        };
        let wait = match err.kind {
            AppErrorKind::RateLimited { retry_after } if !waited_rate_limit => {
                waited_rate_limit = true;
                Duration::from_secs(
                    retry_after
                        .unwrap_or(DEFAULT_RETRY_AFTER)
                        .min(MAX_RETRY_AFTER),
                )
            }
            AppErrorKind::Network | AppErrorKind::Timeout | AppErrorKind::Tls => {
                match backoff.next() {
                    Some(d) => *d,
                    None => return Err(err),
                }
            }
            AppErrorKind::Http(code) if code >= 500 => match backoff.next() {
                Some(d) => *d,
                None => return Err(err),
            },
            _ => return Err(err),
        };
        tracing::info!(error = %err, wait_s = wait.as_secs(), "provider request retried");
        tokio::time::sleep(wait).await;
    }
}

/// Envelope rules (§5.1): 2xx + JSON + `data` present = success. Anything
/// else maps to an error carrying the (redacted, truncated) server message.
pub fn data_of(raw: &Raw) -> AppResult<serde_json::Value> {
    let envelope = serde_json::from_slice::<Envelope>(&raw.body).ok();
    let message = envelope
        .as_ref()
        .and_then(|e| e.message.as_deref())
        .map(truncate)
        .filter(|m| !m.is_empty());
    if !raw.status.is_success() {
        return Err(status_error(raw, message.as_deref()));
    }
    let Some(envelope) = envelope else {
        return Err(compat("非 JSON 响应"));
    };
    match envelope.data {
        Some(data) if !data.is_null() => Ok(data),
        _ => Err(compat(&format!(
            "响应中没有 data{}",
            message.map(|m| format!("：{m}")).unwrap_or_default()
        ))),
    }
}

/// Error for a non-2xx answer (§8 table).
pub fn status_error(raw: &Raw, message: Option<&str>) -> AppError {
    let code = raw.status.as_u16();
    let suffix = message.map(|m| format!("：{m}")).unwrap_or_default();
    match code {
        401 | 403 => AppError::new(
            AppErrorKind::AuthInvalid,
            STAGE,
            format!("服务器返回 HTTP {code}{suffix}，请重新登录"),
        ),
        429 => {
            let retry_after = raw
                .headers
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            AppError::new(
                AppErrorKind::RateLimited { retry_after },
                STAGE,
                format!("服务器返回 HTTP 429{suffix}"),
            )
        }
        567 => AppError::new(
            AppErrorKind::ProviderCompat,
            STAGE,
            "服务端拒绝了客户端标识（HTTP 567）",
        ),
        _ => AppError::new(
            AppErrorKind::Http(code),
            STAGE,
            format!("服务器返回 HTTP {code}{suffix}"),
        ),
    }
}

/// Server message extracted from an error body, if the body is an envelope.
pub fn message_of(raw: &Raw) -> Option<String> {
    serde_json::from_slice::<Envelope>(&raw.body)
        .ok()
        .and_then(|e| e.message)
        .map(|m| truncate(&m))
        .filter(|m| !m.is_empty())
}

pub fn decode<T: DeserializeOwned>(data: serde_json::Value, what: &str) -> AppResult<T> {
    serde_json::from_value(data).map_err(|_| compat(&format!("{what}的结构不符合预期")))
}

/// Field names of an object, for the acceptance record (never values).
pub fn field_names(data: &serde_json::Value) -> String {
    match data {
        serde_json::Value::Object(map) => map.keys().cloned().collect::<Vec<_>>().join(","),
        other => format!("<{}>", json_type(other)),
    }
}

fn json_type(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

pub fn compat(detail: &str) -> AppError {
    AppError::new(AppErrorKind::ProviderCompat, STAGE, detail)
}

fn truncate(text: &str) -> String {
    text.trim().chars().take(MESSAGE_LIMIT).collect()
}

/// Lenient numeric fields (§5.2): numbers, numeric strings, or null.
pub mod lenient {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    fn as_f64(v: &Value) -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn u64_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        let v = Option::<Value>::deserialize(d)?;
        Ok(v.as_ref()
            .and_then(as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|n| n as u64))
    }

    pub fn bool_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
        let v = Option::<Value>::deserialize(d)?;
        Ok(match v {
            Some(Value::Bool(b)) => Some(b),
            Some(other) => as_f64(&other).map(|n| n != 0.0),
            None => None,
        })
    }
}
