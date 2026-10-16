//! Generic subscription downloader (DESIGN.md §8 step 2, §12). A 2xx status
//! alone is not success: the content is still checked by `detect`.

use super::userinfo::{self, SubscriptionMeta};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::EgressPolicy;
use crate::net::{self, EgressRoute, Purpose};
use reqwest::Url;
use reqwest::header::{ACCEPT, USER_AGENT};

const STAGE: &str = "subscriptions.fetch";

/// Provider subscriptions (ziyoumao) are fetched without following redirects
/// (ZIYOUMAO.md §7.2); otherwise the limits match plain subscriptions.
const NO_REDIRECT: Purpose = Purpose {
    max_redirects: 0,
    ..net::SUBSCRIPTION
};

pub struct Downloaded {
    pub raw: Vec<u8>,
    pub meta: SubscriptionMeta,
}

/// What to download (DESIGN.md §6.1 `DownloadDescriptor`, reduced to what
/// the downloader needs). Only headers listed here are sent: no cookies, no
/// account Authorization.
#[derive(Clone)]
pub struct Descriptor {
    pub url: Url,
    pub user_agent: String,
    pub follow_redirects: bool,
}

impl Descriptor {
    pub fn plain(url: Url, user_agent: String) -> Self {
        Self {
            url,
            user_agent,
            follow_redirects: true,
        }
    }
}

/// Downloads honouring the egress policy. `core_port` is the mixed port of a
/// running core, if any (§12.2).
pub async fn download(
    desc: &Descriptor,
    policy: EgressPolicy,
    core_port: Option<u16>,
) -> AppResult<Downloaded> {
    net::check_url(&desc.url)?;
    net::with_egress(policy, core_port, |route| once(desc, route)).await
}

async fn once(desc: &Descriptor, route: EgressRoute) -> AppResult<Downloaded> {
    let purpose = if desc.follow_redirects {
        &net::SUBSCRIPTION
    } else {
        &NO_REDIRECT
    };
    let client = net::client(purpose, route)?;
    let resp = client
        .get(desc.url.clone())
        .header(USER_AGENT, &desc.user_agent)
        .header(ACCEPT, "*/*")
        .send()
        .await
        .map_err(net::map_error)?;
    let status = resp.status();
    if !status.is_success() {
        let code = status.as_u16();
        let (kind, detail) = match code {
            // ziyoumao answers an unexpected client UA with 567 (ZIYOUMAO.md §8).
            567 => (
                AppErrorKind::ProviderCompat,
                "服务端拒绝了客户端标识（HTTP 567）".to_owned(),
            ),
            429 => (
                AppErrorKind::RateLimited {
                    retry_after: resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse().ok()),
                },
                "服务器返回 HTTP 429".to_owned(),
            ),
            _ if status.is_redirection() => (
                AppErrorKind::Http(code),
                format!("服务器返回 HTTP {code}（不跟随重定向）"),
            ),
            _ => (AppErrorKind::Http(code), format!("服务器返回 HTTP {code}")),
        };
        return Err(AppError::new(kind, STAGE, detail));
    }
    let meta = userinfo::parse(resp.headers());
    let raw = net::read_bounded(resp, purpose.max_body).await?;
    tracing::info!(
        host = desc.url.host_str().unwrap_or_default(),
        bytes = raw.len(),
        via_core = matches!(route, EgressRoute::ViaLocalCore(_)),
        "subscription downloaded"
    );
    Ok(Downloaded { raw, meta })
}
