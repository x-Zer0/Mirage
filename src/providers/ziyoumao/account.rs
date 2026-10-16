//! `GET user/info` and `GET user/getSubscribe` → [`AccountSnapshot`] and the
//! subscription URL (ZIYOUMAO.md §5.2, §7).

use super::api::{self, Egress, lenient};
use crate::domain::error::AppResult;
use crate::domain::model::{AccountSnapshot, now};
use crate::domain::redact::SecretString;
use crate::net;
use reqwest::Url;
use serde::Deserialize;

#[derive(Deserialize)]
struct UserInfo {
    email: Option<String>,
    #[serde(default, deserialize_with = "lenient::u64_opt")]
    transfer_enable: Option<u64>,
    #[serde(default, deserialize_with = "lenient::u64_opt")]
    expired_at: Option<u64>,
    #[serde(default, deserialize_with = "lenient::bool_opt")]
    banned: Option<bool>,
}

#[derive(Deserialize)]
struct PlanInfo {
    name: Option<String>,
}

#[derive(Deserialize)]
struct SubscribeInfo {
    subscribe_url: Option<String>,
    #[serde(default, deserialize_with = "lenient::u64_opt")]
    u: Option<u64>,
    #[serde(default, deserialize_with = "lenient::u64_opt")]
    d: Option<u64>,
    #[serde(default, deserialize_with = "lenient::u64_opt")]
    transfer_enable: Option<u64>,
    #[serde(default, deserialize_with = "lenient::u64_opt")]
    expired_at: Option<u64>,
    plan: Option<PlanInfo>,
    #[serde(default, deserialize_with = "lenient::u64_opt")]
    alive_ip: Option<u64>,
}

/// `user/info`: the session check (§6.4). Errors keep their kind, so the
/// caller can tell AuthInvalid from a network failure.
pub async fn user_info(base: &Url, session: &str, egress: Egress) -> AppResult<AccountSnapshot> {
    let data = api::get_data(&api::endpoint(base, "user/info")?, Some(session), egress).await?;
    tracing::debug!(fields = %api::field_names(&data), "ziyoumao user/info");
    let info: UserInfo = api::decode(data, "账号信息")?;
    Ok(AccountSnapshot {
        email_masked: info.email.as_deref().map(mask_email),
        total_bytes: info.transfer_enable,
        expires_at: timestamp(info.expired_at),
        banned: info.banned,
        fetched_at: now(),
        ..Default::default()
    })
}

pub struct Subscription {
    pub url: SecretString,
    /// Usage and plan from the same response.
    pub snapshot: AccountSnapshot,
}

/// `user/getSubscribe` (§5, §7.2): the latest subscription URL plus usage.
pub async fn subscription(base: &Url, session: &str, egress: Egress) -> AppResult<Subscription> {
    let data = api::get_data(
        &api::endpoint(base, "user/getSubscribe")?,
        Some(session),
        egress,
    )
    .await?;
    tracing::debug!(fields = %api::field_names(&data), "ziyoumao user/getSubscribe");
    let info: SubscribeInfo = api::decode(data, "订阅信息")?;
    let url = info
        .subscribe_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| api::compat("订阅信息中没有订阅地址"))?;
    check_subscribe_url(url)?;
    let used = match (info.u, info.d) {
        (None, None) => None,
        (u, d) => Some(u.unwrap_or(0).saturating_add(d.unwrap_or(0))),
    };
    Ok(Subscription {
        url: SecretString::new(url),
        snapshot: AccountSnapshot {
            plan_name: info.plan.and_then(|p| p.name).filter(|n| !n.is_empty()),
            used_bytes: used,
            total_bytes: info.transfer_enable,
            expires_at: timestamp(info.expired_at),
            device_count: info.alive_ip.and_then(|n| u32::try_from(n).ok()),
            fetched_at: now(),
            ..Default::default()
        },
    })
}

/// §7.1: only `https://`, a domain host and no userinfo; the host itself may
/// change and is not pinned. The token query is expected.
pub fn check_subscribe_url(url: &str) -> AppResult<Url> {
    let parsed = net::parse_url(url)?;
    net::check_domain_url(&parsed, true)?;
    Ok(parsed)
}

/// Combines `user/info` and `user/getSubscribe` (the latter wins for usage).
pub fn merge(info: Option<AccountSnapshot>, sub: &AccountSnapshot) -> AccountSnapshot {
    let info = info.unwrap_or_default();
    AccountSnapshot {
        email_masked: info.email_masked,
        plan_name: sub.plan_name.clone(),
        used_bytes: sub.used_bytes,
        total_bytes: sub.total_bytes.or(info.total_bytes),
        expires_at: sub.expires_at.or(info.expires_at),
        device_count: sub.device_count,
        banned: info.banned,
        fetched_at: sub.fetched_at,
    }
}

/// 0 or missing = "not provided" (§5.2), never "permanent".
fn timestamp(v: Option<u64>) -> Option<i64> {
    v.filter(|n| *n > 0).and_then(|n| i64::try_from(n).ok())
}

/// `a***@domain` (DESIGN.md §14.3).
pub fn mask_email(email: &str) -> String {
    match email.trim().split_once('@') {
        Some((user, domain)) => {
            let first: String = user.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".into(),
    }
}
