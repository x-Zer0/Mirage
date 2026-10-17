//! Account subscription adapters. Local files and URL subscriptions use the
//! generic pipeline; provider protocols are dispatched only at this boundary.

pub mod ziyoumao;

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{AccountSnapshot, ConfigRevision, DownloadFormat, EgressPolicy};
use crate::domain::redact::{Password, SecretString};
use crate::storage::layout::Layout;
use crate::subscriptions::fetch::Descriptor;

#[derive(Debug, Clone, Copy)]
pub struct Egress {
    pub policy: EgressPolicy,
    pub core_port: Option<u16>,
}

pub struct LoggedIn {
    pub session: SecretString,
    pub email_masked: String,
    pub identity: SecretString,
    pub trusted_host: String,
    pub snapshot: Option<AccountSnapshot>,
    pub subscribe_url: Option<SecretString>,
    /// A follow-up request failed, but the login session is still valid.
    pub follow_up_error: Option<AppError>,
}

pub struct Refreshed {
    pub snapshot: AccountSnapshot,
    pub identity: Option<SecretString>,
    pub subscribe_url: SecretString,
}

/// Register each account provider here; the actor consumes common results.
#[derive(Debug, Clone, Copy)]
pub enum AccountProvider {
    Ziyoumao,
}

pub fn account_provider(id: &str) -> AppResult<AccountProvider> {
    match id {
        ziyoumao::PROVIDER_ID => Ok(AccountProvider::Ziyoumao),
        _ => Err(AppError::new(
            AppErrorKind::ProviderCompat,
            "providers",
            "尚未支持该账号订阅类型",
        )),
    }
}

pub fn account_name(id: &str) -> &'static str {
    account_provider(id).map_or("服务商", AccountProvider::name)
}

impl AccountProvider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ziyoumao => "自由猫",
        }
    }

    pub async fn login(
        self,
        layout: &Layout,
        email: String,
        password: Password,
        egress: Egress,
    ) -> AppResult<LoggedIn> {
        match self {
            Self::Ziyoumao => ziyoumao::login(layout, email, password, egress).await,
        }
    }

    pub async fn refresh(
        self,
        layout: &Layout,
        hosts: &[String],
        session: &str,
        egress: Egress,
    ) -> AppResult<Refreshed> {
        match self {
            Self::Ziyoumao => ziyoumao::refresh(layout, hosts, session, egress).await,
        }
    }

    pub fn descriptor(self, url: &SecretString) -> AppResult<Descriptor> {
        match self {
            Self::Ziyoumao => ziyoumao::descriptor(url),
        }
    }

    pub fn needs_revision_update(self, revision: Option<&ConfigRevision>) -> bool {
        match self {
            Self::Ziyoumao => {
                revision.is_none_or(|r| r.download_format != DownloadFormat::ZiyoumaoAes)
            }
        }
    }

    pub fn import_legacy(self) -> AppResult<Vec<u8>> {
        match self {
            Self::Ziyoumao => {
                ziyoumao::legacy_import::read(&ziyoumao::legacy_import::default_path()?)
            }
        }
    }
}

/// Provider transport envelopes are decoded before entering the generic pipeline.
pub fn decode_subscription(format: DownloadFormat, raw: Vec<u8>) -> AppResult<Vec<u8>> {
    match format {
        DownloadFormat::Auto => Ok(raw),
        DownloadFormat::ZiyoumaoAes => ziyoumao::subscription::decode(&raw),
    }
}

pub fn is_unreachable(e: &AppError) -> bool {
    matches!(
        e.kind,
        AppErrorKind::Network | AppErrorKind::Timeout | AppErrorKind::Tls
    ) || matches!(e.kind, AppErrorKind::Http(code) if code >= 500)
}
