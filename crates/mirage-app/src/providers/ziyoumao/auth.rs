//! Login (ZIYOUMAO.md §6). One POST to one selected host, never retried,
//! never redirected; the password only lives in a zeroed buffer.

use super::STAGE;
use super::api;
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::redact::{Password, SecretString};
use crate::net::EgressRoute;
use reqwest::{Method, Url};
use serde::Serialize;
use zeroize::Zeroizing;

/// Words in a login error that mean "a step only the user can do" (§6.3).
const USER_ACTION_HINTS: [&str; 4] = ["验证码", "人机", "captcha", "设备"];

#[derive(Serialize)]
struct LoginBody<'a> {
    email: &'a str,
    password: &'a str,
}

/// `POST passport/auth/login` with a JSON body (§6.1); returns `auth_data`.
/// Errors: `LoginRejected` only for a 4xx carrying a server message,
/// `NeedsUserAction` when that message asks for a captcha/device step;
/// anything unknown keeps its HTTP/network kind so it is never shown as
/// "wrong password".
pub async fn login(
    base: &Url,
    email: &str,
    password: Password,
    route: EgressRoute,
) -> AppResult<SecretString> {
    let url = api::endpoint(base, "passport/auth/login")?;
    let body = encode_body(email, &password)?;
    drop(password);
    let raw = api::send(Method::POST, &url, None, Some(body), route).await?;
    if !raw.status.is_success() {
        return Err(login_error(&raw));
    }
    let data = api::data_of(&raw)?;
    // The first real login records the field names (never values) so the
    // acceptance notes can confirm the response shape (§10 step 2).
    tracing::info!(
        status = raw.status.as_u16(),
        fields = %api::field_names(&data),
        "ziyoumao login response"
    );
    match data.get("auth_data").and_then(|v| v.as_str()) {
        Some(token) if !token.trim().is_empty() => Ok(SecretString::new(token.trim())),
        _ => Err(api::compat("登录响应中没有会话令牌（auth_data）")),
    }
}

/// JSON body in a pre-sized, zeroed-on-drop buffer: no reallocation leaves a
/// copy of the password behind.
fn encode_body(email: &str, password: &Password) -> AppResult<bytes::Bytes> {
    let capacity = 64 + email.len() * 6 + password.expose().len() * 6;
    let mut buf = Zeroizing::new(Vec::with_capacity(capacity));
    serde_json::to_writer(
        &mut *buf,
        &LoginBody {
            email,
            password: password.expose(),
        },
    )
    .map_err(|_| AppError::internal(STAGE, "无法构造登录请求"))?;
    Ok(bytes::Bytes::from_owner(buf))
}

fn login_error(raw: &api::Raw) -> AppError {
    let code = raw.status.as_u16();
    let message = api::message_of(raw);
    tracing::warn!(
        status = code,
        has_message = message.is_some(),
        body_is_json = serde_json::from_slice::<serde_json::Value>(&raw.body).is_ok(),
        "ziyoumao login refused"
    );
    match (code, message) {
        (429, _) => api::status_error(raw, None),
        // 【已验证 M4】the panel answers a wrong password with HTTP 500 and a
        // JSON envelope message (V2Board `abort(500, …)`); the message is
        // shown verbatim. 5xx without an envelope message stays an HTTP error.
        (500, Some(m)) if needs_user_action(&m) => {
            AppError::new(AppErrorKind::NeedsUserAction, STAGE, m)
        }
        (500, Some(m)) => AppError::new(AppErrorKind::LoginRejected, STAGE, m),
        (500.., m) => api::status_error(raw, m.as_deref()),
        (400..=499, Some(m)) if needs_user_action(&m) => {
            AppError::new(AppErrorKind::NeedsUserAction, STAGE, m)
        }
        (415 | 422, m) => AppError::new(
            AppErrorKind::ProviderCompat,
            STAGE,
            format!(
                "服务端不接受登录请求的格式（HTTP {code}）{}",
                m.map(|m| format!("：{m}")).unwrap_or_default()
            ),
        ),
        (400..=499, Some(m)) => AppError::new(AppErrorKind::LoginRejected, STAGE, m),
        _ => api::status_error(raw, None),
    }
}

fn needs_user_action(message: &str) -> bool {
    let lower = message.to_lowercase();
    USER_ACTION_HINTS.iter().any(|h| lower.contains(h))
}
