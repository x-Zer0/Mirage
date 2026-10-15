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
}

impl SubscriptionSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LocalFile { .. } => "本地文件",
        }
    }
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
}
