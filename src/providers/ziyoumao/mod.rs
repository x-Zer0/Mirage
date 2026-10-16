//! ziyoumao (自由猫) account provider (ZIYOUMAO.md). Produces account
//! snapshots and subscription download descriptors; it never starts the
//! core, touches the system proxy or opens windows.
//!
//! Like `cores::mihomo`, this is a concrete module: the
//! `ProviderAccountAdapter` trait of DESIGN.md §6.1 is extracted once a
//! second provider exists.

mod account;
mod api;
mod auth;
pub mod bootstrap;
pub mod legacy_import;

pub use api::Egress;

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::AccountSnapshot;
use crate::domain::redact::{Password, SecretString};
use crate::storage::layout::Layout;
use crate::subscriptions::fetch::Descriptor;
use reqwest::Url;

pub const PROVIDER_ID: &str = "ziyoumao";
const STAGE: &str = "ziyoumao";
/// §7.2: exactly this UA (no version suffix); `Clash.Meta` gets HTTP 567.
pub const SUBSCRIPTION_UA: &str = "clash-verge";

pub struct LoggedIn {
    pub session: SecretString,
    pub email_masked: String,
    /// Origin of the host the session is bound to (§4.3).
    pub trusted_host: String,
    pub snapshot: Option<AccountSnapshot>,
    pub subscribe_url: Option<SecretString>,
    /// Account calls after the login that failed (the session is still kept).
    pub follow_up_error: Option<AppError>,
}

/// discover → login → user/info + user/getSubscribe (DESIGN.md §16.3).
pub async fn login(
    layout: &Layout,
    email: String,
    password: Password,
    egress: Egress,
) -> AppResult<LoggedIn> {
    let email = email.trim().to_owned();
    if email.is_empty() || !email.contains('@') || password.expose().is_empty() {
        drop(password);
        return Err(AppError::new(
            AppErrorKind::LoginRejected,
            STAGE,
            "请输入邮箱和密码",
        ));
    }
    let discovery = match bootstrap::discover(layout, egress).await {
        Ok(d) => d,
        Err(e) => {
            drop(password);
            return Err(e);
        }
    };
    let (base, comm) = match bootstrap::require_selected(&discovery) {
        Ok(s) => s,
        Err(e) => {
            drop(password);
            return Err(e);
        }
    };
    let session = auth::login(&base, &email, password, comm.route)
        .await
        .map_err(|e| with_recaptcha_hint(e, comm.recaptcha))?;
    tracing::info!(
        host = base.host_str().unwrap_or_default(),
        "ziyoumao login succeeded"
    );
    let mut logged_in = LoggedIn {
        session,
        email_masked: account::mask_email(&email),
        trusted_host: bootstrap::origin(&base),
        snapshot: None,
        subscribe_url: None,
        follow_up_error: None,
    };
    match fetch_account(&base, logged_in.session.expose(), egress).await {
        Ok((snapshot, url)) => {
            logged_in.snapshot = Some(snapshot);
            logged_in.subscribe_url = Some(url);
        }
        Err(e) => logged_in.follow_up_error = Some(e),
    }
    Ok(logged_in)
}

/// Fresh account data and the latest subscription URL (§6.4, §7.2).
pub struct Refreshed {
    pub snapshot: AccountSnapshot,
    pub subscribe_url: SecretString,
}

/// Validates the saved session against the trusted hosts only (§4.3). When
/// none is reachable and the bootstrap now lists other hosts, the error says
/// the service moved and a new login is needed.
pub async fn refresh(
    layout: &Layout,
    trusted_hosts: &[String],
    session: &str,
    egress: Egress,
) -> AppResult<Refreshed> {
    let mut last_err = None;
    for host in trusted_hosts {
        let Some(base) = api_base(host) else {
            continue;
        };
        match fetch_account(&base, session, egress).await {
            Ok((snapshot, subscribe_url)) => {
                return Ok(Refreshed {
                    snapshot,
                    subscribe_url,
                });
            }
            Err(e) if is_unreachable(&e) => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    let err = last_err
        .unwrap_or_else(|| AppError::new(AppErrorKind::Network, STAGE, "账号没有可信的服务地址"));
    Err(moved_hint(layout, trusted_hosts, egress, err).await)
}

/// Download descriptor for a subscription URL (§7.2): UA `clash-verge`, no
/// redirects, no account Authorization.
pub fn descriptor(url: &SecretString) -> AppResult<Descriptor> {
    Ok(Descriptor {
        url: account::check_subscribe_url(url.expose())?,
        user_agent: SUBSCRIPTION_UA.into(),
        follow_redirects: false,
    })
}

/// Errors that leave the account usable, just not reachable right now.
pub fn is_unreachable(e: &AppError) -> bool {
    matches!(
        e.kind,
        AppErrorKind::Network | AppErrorKind::Timeout | AppErrorKind::Tls
    ) || matches!(e.kind, AppErrorKind::Http(code) if code >= 500)
}

async fn fetch_account(
    base: &Url,
    session: &str,
    egress: Egress,
) -> AppResult<(AccountSnapshot, SecretString)> {
    let info = account::user_info(base, session, egress).await?;
    let sub = account::subscription(base, session, egress).await?;
    Ok((account::merge(Some(info), &sub.snapshot), sub.url))
}

async fn moved_hint(
    layout: &Layout,
    trusted_hosts: &[String],
    egress: Egress,
    err: AppError,
) -> AppError {
    let Ok(discovery) = bootstrap::discover(layout, egress).await else {
        return err;
    };
    let moved = discovery
        .candidates
        .iter()
        .any(|base| !trusted_hosts.iter().any(|h| bootstrap::same_host(base, h)))
        && discovery
            .selected
            .as_ref()
            .is_some_and(|(base, _)| !trusted_hosts.iter().any(|h| bootstrap::same_host(base, h)));
    if moved {
        err.append("；服务地址已变更，请重新登录")
    } else {
        err
    }
}

fn api_base(host: &str) -> Option<Url> {
    Url::parse(&format!("{}/api/v1/", host.trim_end_matches('/'))).ok()
}

fn with_recaptcha_hint(e: AppError, recaptcha: bool) -> AppError {
    if recaptcha && e.kind != AppErrorKind::LoginRejected {
        e.append("（该服务可能需要人机验证，请稍后重试或使用原客户端完成验证）")
    } else {
        e
    }
}
