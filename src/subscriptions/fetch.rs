//! Generic subscription downloader (DESIGN.md §8 step 2, §12). A 2xx status
//! alone is not success: the content is still checked by `detect`.

use super::userinfo::{self, SubscriptionMeta};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::EgressPolicy;
use crate::net::{self, EgressRoute};
use reqwest::Url;
use reqwest::header::{ACCEPT, USER_AGENT};

const STAGE: &str = "subscriptions.fetch";

pub struct Downloaded {
    pub raw: Vec<u8>,
    pub meta: SubscriptionMeta,
}

/// Downloads `url` honouring the egress policy. `core_port` is the mixed port
/// of a running core, if any (§12.2).
pub async fn download(
    url: &Url,
    user_agent: &str,
    policy: EgressPolicy,
    core_port: Option<u16>,
) -> AppResult<Downloaded> {
    net::check_url(url)?;
    match (policy, core_port) {
        (EgressPolicy::ViaLocalCore, Some(port)) => {
            once(url, user_agent, EgressRoute::ViaLocalCore(port)).await
        }
        (EgressPolicy::ViaLocalCore, None) => Err(AppError::new(
            AppErrorKind::Network,
            STAGE,
            "已设置为经本地内核更新，但内核未运行",
        )),
        (EgressPolicy::Direct, _) | (EgressPolicy::Auto, None) => {
            once(url, user_agent, EgressRoute::Direct).await
        }
        (EgressPolicy::Auto, Some(port)) => {
            match once(url, user_agent, EgressRoute::Direct).await {
                Err(e) if net::is_transport_failure(&e) => {
                    tracing::info!(error = %e, "direct download failed, retrying via local core");
                    once(url, user_agent, EgressRoute::ViaLocalCore(port)).await
                }
                other => other,
            }
        }
    }
}

async fn once(url: &Url, user_agent: &str, route: EgressRoute) -> AppResult<Downloaded> {
    let client = net::client(&net::SUBSCRIPTION, route)?;
    let resp = client
        .get(url.clone())
        .header(USER_AGENT, user_agent)
        .header(ACCEPT, "*/*")
        .send()
        .await
        .map_err(net::map_error)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::new(
            AppErrorKind::Http(status.as_u16()),
            STAGE,
            format!("服务器返回 HTTP {}", status.as_u16()),
        ));
    }
    let meta = userinfo::parse(resp.headers());
    let raw = net::read_bounded(resp, net::SUBSCRIPTION.max_body).await?;
    tracing::info!(
        host = url.host_str().unwrap_or_default(),
        bytes = raw.len(),
        via_core = matches!(route, EgressRoute::ViaLocalCore(_)),
        "subscription downloaded"
    );
    Ok(Downloaded { raw, meta })
}
