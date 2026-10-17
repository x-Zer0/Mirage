//! Persistent domain records (DESIGN.md §5). IDs are local UUID v4 strings.

use serde::{Deserialize, Serialize};

pub type ProfileId = String;
pub type RevisionId = String;
pub type AccountId = String;
/// Compiled-in provider module id, e.g. `"ziyoumao"`.
pub type ProviderId = String;

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Unix seconds; enough precision for display and ordering.
pub type Timestamp = i64;

pub fn now() -> Timestamp {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubscriptionSource {
    LocalFile {
        original_name: String,
        imported_at: Timestamp,
    },
    /// The full URL lives in the SecretStore under `url_secret` (§14.1).
    RemoteUrl {
        url_secret: SecretRef,
        /// Host only, for display; never the full URL.
        host: String,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// Subscription discovered through a provider account (DESIGN.md §6.2);
    /// the URL is resolved by the provider module before every update.
    ProviderAccount {
        provider_id: ProviderId,
        account_id: AccountId,
    },
}

impl SubscriptionSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LocalFile { .. } => "本地文件",
            Self::RemoteUrl { .. } => "订阅链接",
            Self::ProviderAccount { .. } => "自由猫账号",
        }
    }
}

/// Opaque SecretStore key, e.g. `profile/<uuid>/url`.
pub type SecretRef = String;

/// Default UA for plain subscriptions (§15): many services pick the YAML
/// variant only when the UA names clash/mihomo.
pub const DEFAULT_SUBSCRIPTION_UA: &str = "clash-verge/v2.0.0 mihomo/1.19.32";
pub const DEFAULT_UPDATE_HOURS: u32 = 24;

/// How subscription downloads leave the machine (§12.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressPolicy {
    Direct,
    ViaLocalCore,
    /// Direct first; on a network error retry once through a running core.
    #[default]
    Auto,
}

impl EgressPolicy {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Direct, Self::ViaLocalCore];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动（先直连，失败再经本地内核）",
            Self::Direct => "直连",
            Self::ViaLocalCore => "经本地内核",
        }
    }
}

/// Mihomo `mode` (DESIGN.md §9.2, §10.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    Rule,
    Global,
    Direct,
}

impl RoutingMode {
    pub const ALL: [Self; 3] = [Self::Rule, Self::Global, Self::Direct];

    /// Value of the `mode` key / `PATCH /configs`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(text))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rule => "规则",
            Self::Global => "全局",
            Self::Direct => "直连",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePolicy {
    /// 0 = manual only.
    pub interval_hours: u32,
    /// The user chose the interval; a server `profile-update-interval` no longer applies.
    #[serde(default)]
    pub user_set: bool,
    #[serde(default)]
    pub egress: EgressPolicy,
}

impl Default for UpdatePolicy {
    fn default() -> Self {
        Self {
            interval_hours: DEFAULT_UPDATE_HOURS,
            user_set: false,
            egress: EgressPolicy::Auto,
        }
    }
}

/// From `subscription-userinfo` (§15). Bytes and Unix seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageInfo {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    /// None when missing or 0 ("not provided", not "never expires").
    pub expire: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchRecord {
    pub at: Timestamp,
    /// Redacted reason; None on success.
    pub error: Option<String>,
}

/// Transport envelope, separate from the normalized configuration format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadFormat {
    #[default]
    Auto,
    ZiyoumaoAes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFormat {
    MihomoYaml,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum Validity {
    Unchecked,
    Valid,
    Invalid(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSummary {
    pub proxies: usize,
    pub groups: usize,
    pub rules: usize,
    pub proxy_providers: usize,
    pub rule_providers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigRevision {
    pub id: RevisionId,
    pub raw_sha256: String,
    pub format: ConfigFormat,
    /// Envelope used to obtain the normalized raw YAML; old revisions default to Auto.
    #[serde(default)]
    pub download_format: DownloadFormat,
    pub fetched_at: Timestamp,
    pub checked_with: Option<String>,
    pub overlay_version: u32,
    pub validity: Validity,
    pub summary: ConfigSummary,
    /// Keys the composer removed or rewrote (DESIGN.md §9.5).
    #[serde(default)]
    pub policy_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub source: SubscriptionSource,
    pub active_revision: Option<RevisionId>,
    pub last_good_revision: Option<RevisionId>,
    pub updated_at: Timestamp,
    #[serde(default)]
    pub update_policy: UpdatePolicy,
    #[serde(default)]
    pub usage: Option<UsageInfo>,
    #[serde(default)]
    pub last_fetch: Option<FetchRecord>,
}

impl Profile {
    /// Fetched from the network (URL or provider account), so it can be updated.
    pub fn is_remote(&self) -> bool {
        !matches!(self.source, SubscriptionSource::LocalFile { .. })
    }
}

/// DESIGN.md §5.1; independent of the core and subscription states.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum AccountStatus {
    #[default]
    LoggedOut,
    LoggingIn,
    Validating,
    Valid,
    /// The server rejected the saved session; log in again.
    ReauthRequired,
    /// Captcha / device confirmation; the text tells the user what to do.
    NeedsUserAction(String),
    /// Hosts unreachable; the session and the old config are kept.
    NetworkUnavailable(String),
}

/// Account data from the provider API (DESIGN.md §6.1). `None` = not provided.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub email_masked: Option<String>,
    pub plan_name: Option<String>,
    pub used_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// None or 0 from the server means "not provided", never "permanent".
    pub expires_at: Option<Timestamp>,
    pub device_count: Option<u32>,
    pub banned: Option<bool>,
    pub fetched_at: Timestamp,
}

impl AccountSnapshot {
    /// Provider usage wins over subscription headers (§15).
    pub fn usage(&self) -> Option<UsageInfo> {
        if self.used_bytes.is_none() && self.total_bytes.is_none() && self.expires_at.is_none() {
            return None;
        }
        Some(UsageInfo {
            upload: 0,
            download: self.used_bytes.unwrap_or(0),
            total: self.total_bytes.unwrap_or(0),
            expire: self.expires_at,
        })
    }
}

/// `accounts.json` entry (DESIGN.md §5). Secrets are only referenced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAccount {
    pub id: AccountId,
    pub provider_id: ProviderId,
    /// Masked e-mail, e.g. `a***@example.com`.
    pub display_name: String,
    pub session_secret: Option<SecretRef>,
    pub subscribe_secret: Option<SecretRef>,
    /// API bases the session may be sent to (ZIYOUMAO.md §4.3).
    pub trusted_hosts: Vec<String>,
    pub status: AccountStatus,
    pub snapshot: Option<AccountSnapshot>,
    /// Profile created from this account's subscription.
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    pub created_at: Timestamp,
}
