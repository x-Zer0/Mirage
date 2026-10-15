//! Outbound HTTP for the app itself (DESIGN.md §12). Every client ignores the
//! system proxy (it may point at our own, possibly dead, core), keeps no
//! cookies, verifies TLS against the Windows store and reads bodies with a
//! hard size limit.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use reqwest::{Client, Url};
use std::time::Duration;

const STAGE: &str = "net";

/// How a request leaves the machine (§12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressRoute {
    Direct,
    /// `http://127.0.0.1:<mixed-port>`; rules of the running config apply.
    ViaLocalCore(u16),
}

/// Per-purpose limits of the client factory (§12.1).
pub struct Purpose {
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    pub max_body: usize,
    /// Number of redirects followed (https only); 0 = none.
    pub max_redirects: usize,
}

pub const SUBSCRIPTION: Purpose = Purpose {
    connect_timeout: Duration::from_secs(10),
    total_timeout: Duration::from_secs(60),
    max_body: 16 * 1024 * 1024,
    max_redirects: 5,
};

pub fn client(purpose: &Purpose, route: EgressRoute) -> AppResult<Client> {
    let max = purpose.max_redirects;
    let redirect = if max == 0 {
        reqwest::redirect::Policy::none()
    } else {
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > max {
                attempt.error("重定向次数过多")
            } else if check_url(attempt.url()).is_err() {
                attempt.error("重定向到了非 https 地址")
            } else {
                attempt.follow()
            }
        })
    };
    let mut builder = Client::builder()
        .no_proxy()
        .connect_timeout(purpose.connect_timeout)
        .timeout(purpose.total_timeout)
        .redirect(redirect);
    if let EgressRoute::ViaLocalCore(port) = route {
        let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))
            .map_err(|e| AppError::internal(STAGE, e.without_url()))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| AppError::internal(STAGE, e.without_url()))
}

/// Accepts `https://` without userinfo. Debug builds also accept loopback
/// `http://127.0.0.1` so the pipeline can be exercised against a local server.
pub fn check_url(url: &Url) -> AppResult<()> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("地址中不能包含用户名或密码"));
    }
    let loopback_dev =
        cfg!(debug_assertions) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if url.scheme() != "https" && !loopback_dev {
        return Err(invalid("只支持 https:// 地址"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(invalid("缺少主机名"));
    }
    Ok(())
}

pub fn parse_url(text: &str) -> AppResult<Url> {
    let url = Url::parse(text.trim()).map_err(|_| invalid("不是有效的网址"))?;
    check_url(&url)?;
    Ok(url)
}

/// Reads the body chunk by chunk and aborts once `max` is exceeded; the
/// Content-Length header is not trusted.
pub async fn read_bounded(mut resp: reqwest::Response, max: usize) -> AppResult<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(map_error)? {
        if body.len() + chunk.len() > max {
            return Err(AppError::new(
                AppErrorKind::ResponseTooLarge,
                STAGE,
                format!("响应超过 {} MiB 上限", max / (1024 * 1024)),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Classifies a reqwest error without ever formatting its URL.
pub fn map_error(e: reqwest::Error) -> AppError {
    let kind = if e.is_timeout() {
        AppErrorKind::Timeout
    } else if e.is_redirect() {
        AppErrorKind::InvalidUrl
    } else if is_tls(&e) {
        AppErrorKind::Tls
    } else {
        AppErrorKind::Network
    };
    AppError::new(kind, STAGE, error_chain(&e.without_url()))
}

/// Whether a direct failure may be retried through the local core (§12.2).
pub fn is_transport_failure(e: &AppError) -> bool {
    matches!(e.kind, AppErrorKind::Network | AppErrorKind::Timeout)
}

fn is_tls(e: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = source {
        if err.downcast_ref::<rustls::Error>().is_some() {
            return true;
        }
        if let Some(io) = err.downcast_ref::<std::io::Error>()
            && io
                .get_ref()
                .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some())
        {
            return true;
        }
        source = err.source();
    }
    false
}

/// `outer: inner: innermost`, so "error sending request" carries its cause.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(err) = source {
        let part = err.to_string();
        if !text.contains(&part) {
            text.push('：');
            text.push_str(&part);
        }
        source = err.source();
    }
    text
}

fn invalid(detail: &str) -> AppError {
    AppError::new(AppErrorKind::InvalidUrl, STAGE, detail)
}
