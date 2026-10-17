//! App adapter for the policy shared with the privileged TUN service.
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{ConfigSummary, ProfileId, RoutingMode};
pub use mirage_ipc::policy::OVERLAY_VERSION;
use serde_yaml_ng::{Mapping, Value};
use std::net::SocketAddr;

const STAGE: &str = "mihomo.compose";

pub struct ManagedOverlay<'a> {
    pub mixed_port: u16,
    pub controller: SocketAddr,
    pub secret: &'a str,
    pub log_level: &'a str,
    pub profile_id: &'a ProfileId,
    pub mode: Option<RoutingMode>,
    pub tun: bool,
}

pub struct Composed {
    pub yaml: String,
    pub summary: ConfigSummary,
    pub policy_notes: Vec<String>,
}

pub fn compose(text: &str, overlay: &ManagedOverlay<'_>) -> AppResult<Composed> {
    let before = summarize(&parse(text)?);
    let (yaml, policy_notes) = mirage_ipc::policy::apply(
        text,
        &mirage_ipc::ManagedOverlay {
            mixed_port: overlay.mixed_port,
            controller: overlay.controller,
            secret: overlay.secret,
            log_level: overlay.log_level,
            mode: overlay.mode.map(RoutingMode::as_str),
            tun: overlay.tun.then(Default::default),
            profile_id: overlay.profile_id,
        },
    )
    .map_err(|e| AppError::new(AppErrorKind::PolicyRejected, STAGE, e))?;
    let summary = summarize(&parse(&yaml)?);
    if summary != before {
        return Err(AppError::internal(STAGE, "合成前后摘要不一致"));
    }
    Ok(Composed {
        yaml,
        summary,
        policy_notes,
    })
}

pub fn parse(text: &str) -> AppResult<Value> {
    mirage_ipc::policy::parse(text).map_err(|e| AppError::new(AppErrorKind::NotAConfig, STAGE, e))
}
pub fn summarize(root: &Value) -> ConfigSummary {
    let seq_len = |k: &str| root.get(k).and_then(Value::as_sequence).map_or(0, Vec::len);
    let map_len = |k: &str| {
        root.get(k)
            .and_then(Value::as_mapping)
            .map_or(0, Mapping::len)
    };
    ConfigSummary {
        proxies: seq_len("proxies"),
        groups: seq_len("proxy-groups"),
        rules: seq_len("rules"),
        proxy_providers: map_len("proxy-providers"),
        rule_providers: map_len("rule-providers"),
    }
}
