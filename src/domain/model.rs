//! Persistent domain records (DESIGN.md §5). IDs are local UUID v4 strings.

use serde::{Deserialize, Serialize};

pub type ProfileId = String;
pub type RevisionId = String;

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
}

impl SubscriptionSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LocalFile { .. } => "本地文件",
            Self::RemoteUrl { .. } => "订阅链接",
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
    pub fn is_remote(&self) -> bool {
        matches!(self.source, SubscriptionSource::RemoteUrl { .. })
    }
}
