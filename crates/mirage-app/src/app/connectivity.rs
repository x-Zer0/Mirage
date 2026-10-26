//! Reachability check through the local core (DESIGN.md §5.1, §16.2): a
//! running core does not mean the internet is reachable, so it is shown apart.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use std::time::{Duration, Instant};

const STAGE: &str = "app.connectivity";
/// HTTPS, matching the delay test: plain HTTP gets hijacked or answered with a
/// Cloudflare challenge on flagged exit IPs even when the node works.
const PROBE_URL: &str = "https://cp.cloudflare.com/generate_204";
const TIMEOUT: Duration = Duration::from_secs(10);

/// DNS and interface selection may still settle just after a TUN restart.
pub const SETTLE_WINDOW: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Retry transient startup failures twice; keep HTTP interception errors visible.
pub async fn probe(mixed_port: u16, settling: bool) -> AppResult<u32> {
    let mut retries = if settling { 2 } else { 0 };
    loop {
        match probe_once(mixed_port).await {
            Err(e) if retries > 0 && is_transient(&e) => {
                tracing::debug!(error = %e, retries, "TUN connectivity settling, retrying");
                retries -= 1;
                tokio::time::sleep(RETRY_DELAY).await;
            }
            result => return result,
        }
    }
}

fn is_transient(error: &AppError) -> bool {
    crate::net::is_transport_failure(error) || matches!(error.kind, AppErrorKind::Http(502..=504))
}

/// Round-trip time excludes startup retries and their delays.
async fn probe_once(mixed_port: u16) -> AppResult<u32> {
    tokio::time::timeout(TIMEOUT, probe_request(mixed_port))
        .await
        .map_err(|_| AppError::new(AppErrorKind::Timeout, STAGE, "连通性检测超时，请重试"))?
}

async fn probe_request(mixed_port: u16) -> AppResult<u32> {
    let purpose = crate::net::Purpose {
        connect_timeout: Duration::from_secs(5),
        total_timeout: TIMEOUT,
        max_body: 0,
        max_redirects: 0,
    };
    let client = crate::net::client(&purpose, crate::net::EgressRoute::ViaLocalCore(mixed_port))?;
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
