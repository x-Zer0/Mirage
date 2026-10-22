//! App adapter for the policy shared with the privileged TUN service.
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{ConfigSummary, Ipv6Mode, ProfileId, RoutingMode};
use crate::overrides::hosts::HostEntries;
pub use mirage_ipc::policy::OVERLAY_VERSION;
use serde_yaml_ng::{Mapping, Value};
use std::net::SocketAddr;

const STAGE: &str = "mihomo.compose";

pub struct ManagedOverlay<'a> {
    pub mixed_port: u16,
    pub allow_lan: bool,
    pub controller: SocketAddr,
    pub secret: &'a str,
    pub log_level: &'a str,
    pub profile_id: &'a ProfileId,
    pub mode: Option<RoutingMode>,
    pub tun: bool,
    /// Applied after the security policy, app side only (iteration 2 §1).
    pub user: &'a UserOverlay,
}

/// User preferences layered over the policy output. Only whitelisted,
/// non-security keys are touched, so the TUN service's second
/// `policy::apply` keeps them. `Default` is a no-op.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserOverlay {
    /// Merged over the subscription's `hosts`; empty when disabled.
    pub hosts: HostEntries,
    pub ipv6: Ipv6Mode,
    /// Forces `unified-delay: true` (delay excludes handshakes); off follows the subscription.
    pub unified_delay: bool,
}

impl UserOverlay {
    pub fn describe(&self) -> String {
        format!(
            "Hosts：{} 条；IPv6：{}；测速去除握手延迟：{}",
            self.hosts.len(),
            self.ipv6.label(),
            if self.unified_delay { "开" } else { "关" }
        )
    }
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
            allow_lan: overlay.allow_lan,
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
    let yaml = apply_user(yaml, overlay.user)?;
    Ok(Composed {
        yaml,
        summary,
        policy_notes,
    })
}

/// Writes the user overlay into the policy output; the summary is unaffected.
fn apply_user(yaml: String, user: &UserOverlay) -> AppResult<String> {
    if *user == UserOverlay::default() {
        return Ok(yaml);
    }
    let mut root = parse(&yaml)?;
    let map = root
        .as_mapping_mut()
        .ok_or_else(|| AppError::internal(STAGE, "配置顶层不是映射"))?;
    if let Some(ipv6) = user.ipv6.forced() {
        map.insert("ipv6".into(), Value::Bool(ipv6));
        // TUN's injected minimal DNS has no `ipv6`, and Mihomo defaults it to false.
        if let Some(dns) = map.get_mut("dns").and_then(Value::as_mapping_mut) {
            dns.insert("ipv6".into(), Value::Bool(ipv6));
        }
    }
    if user.unified_delay {
        map.insert("unified-delay".into(), Value::Bool(true));
    }
    if !user.hosts.is_empty() {
        // Same key: the user's address wins over the subscription's.
        let mut hosts = map
            .get("hosts")
            .and_then(Value::as_mapping)
            .cloned()
            .unwrap_or_default();
        for (domain, ips) in &user.hosts {
            let value = match ips.as_slice() {
                [ip] => Value::from(ip.to_string()),
                ips => Value::Sequence(ips.iter().map(|ip| Value::from(ip.to_string())).collect()),
            };
            hosts.insert(Value::from(domain.as_str()), value);
        }
        map.insert("hosts".into(), Value::Mapping(hosts));
        // Under TUN / fake-ip the answer comes from the DNS module.
        if let Some(dns) = map.get_mut("dns").and_then(Value::as_mapping_mut) {
            dns.insert("use-hosts".into(), Value::Bool(true));
        }
    }
    serde_yaml_ng::to_string(&root).map_err(|e| AppError::internal(STAGE, e))
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
