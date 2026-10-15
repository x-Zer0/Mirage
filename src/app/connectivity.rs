//! Reachability check through the local core (DESIGN.md §5.1, §16.2): a
//! running core does not mean the internet is reachable, so it is shown apart.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use std::time::{Duration, Instant};

const STAGE: &str = "app.connectivity";
/// Plain HTTP keeps this independent of TLS support; the core still has to
/// reach the internet through the selected node for a 204 to come back.
const PROBE_URL: &str = "http://cp.cloudflare.com/generate_204";
const TIMEOUT: Duration = Duration::from_secs(10);

/// Round-trip time in ms of a `generate_204` request sent via the mixed port.
pub async fn probe(mixed_port: u16) -> AppResult<u32> {
    let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{mixed_port}"))
        .map_err(|e| AppError::internal(STAGE, e.without_url()))?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::internal(STAGE, e.without_url()))?;
    let started = Instant::now();
    let resp = client.get(PROBE_URL).send().await.map_err(|e| {
        let kind = if e.is_timeout() {
            AppErrorKind::Timeout
        } else {
            AppErrorKind::Network
        };
        AppError::new(kind, STAGE, e.without_url().to_string())
    })?;
    let status = resp.status().as_u16();
    if status != 204 {
        return Err(AppError::new(
            AppErrorKind::Http(status),
            STAGE,
            "检测地址返回了非 204 响应（可能被拦截或劫持）",
        ));
    }
    Ok(u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX))
}
