//! App adapter for the policy shared with the privileged TUN service.
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{ConfigSummary, Ipv6Mode, ProfileId, RoutingMode};
use crate::overrides::hosts::HostEntries;
use crate::overrides::rules;
pub use mirage_ipc::policy::OVERLAY_VERSION;
use serde_yaml_ng::{Mapping, Value};
use std::collections::HashSet;
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
    /// Validated rule strings prepended to `rules`; empty when disabled.
    pub rules: Vec<String>,
    /// Merged over the subscription's `hosts`; empty when disabled.
    pub hosts: HostEntries,
    pub ipv6: Ipv6Mode,
    /// Forces `unified-delay: true` (delay excludes handshakes); off follows the subscription.
    pub unified_delay: bool,
}

impl UserOverlay {
    pub fn describe(&self) -> String {
        format!(
            "附加规则：{} 条；Hosts：{} 条；IPv6：{}；测速去除握手延迟：{}",
            self.rules.len(),
            self.hosts.len(),
            self.ipv6.label(),
            if self.unified_delay { "开" } else { "关" }
        )
    }
}

pub struct Composed {
    pub yaml: String,
    pub summary: ConfigSummary,
    /// Policy notes, followed by the user overlay's (e.g. skipped rules).
    pub policy_notes: Vec<String>,
    /// Only the user overlay's notes.
    pub user_notes: Vec<String>,
}

pub fn compose(text: &str, overlay: &ManagedOverlay<'_>) -> AppResult<Composed> {
    let before = summarize(&parse(text)?);
    let (yaml, mut policy_notes) = mirage_ipc::policy::apply(
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
    // Parsed once: checks the serialised policy output and is the base the
    // user overlay is written into.
    let root = parse(&yaml)?;
    let summary = summarize(&root);
    if summary != before {
        return Err(AppError::internal(STAGE, "合成前后摘要不一致"));
    }
    let (yaml, user_notes) = apply_user(yaml, root, overlay.user)?;
    policy_notes.extend(user_notes.iter().cloned());
    Ok(Composed {
        yaml,
        summary,
        policy_notes,
        user_notes,
    })
}

/// Writes the user overlay into `root`, the parsed policy output `yaml`.
fn apply_user(
    yaml: String,
    mut root: Value,
    user: &UserOverlay,
) -> AppResult<(String, Vec<String>)> {
    if *user == UserOverlay::default() {
        return Ok((yaml, Vec::new()));
    }
    let mut notes = Vec::new();
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
    if !user.rules.is_empty() {
        prepend_rules(map, &user.rules, &mut notes);
    }
    let yaml = serde_yaml_ng::to_string(&root).map_err(|e| AppError::internal(STAGE, e))?;
    Ok((yaml, notes))
}

/// §3.1: rules whose target, rule provider or sub-rule is missing from this
/// config are skipped rather than failing the whole config. `CURRENT-PROXY`
/// targets, and `PROXY` ones in a config without that name, are rewritten
/// to this config's main group.
fn prepend_rules(map: &mut Mapping, user: &[String], notes: &mut Vec<String>) {
    let names = |key: &str| -> Vec<String> {
        map.get(key)
            .and_then(Value::as_sequence)
            .into_iter()
            .flatten()
            .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect()
    };
    let keys = |key: &str| -> HashSet<String> {
        map.get(key)
            .and_then(Value::as_mapping)
            .into_iter()
            .flat_map(|m| m.keys())
            .filter_map(|k| k.as_str().map(str::to_owned))
            .collect()
    };
    let groups = names("proxy-groups");
    // A name with a comma can't be written into a rule string.
    let main_group = groups.first().filter(|g| !g.contains(',')).cloned();
    let mut targets: HashSet<String> = names("proxies").into_iter().collect();
    targets.extend(groups);
    targets.extend(rules::BUILTIN_TARGETS.iter().map(|t| (*t).to_owned()));
    let providers = keys("rule-providers");
    let sub_rules = keys("sub-rules");

    let mut kept = Vec::with_capacity(user.len());
    // Reported in first-seen order; `seen` keeps the lookup cheap.
    let mut missing: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut process = false;
    for text in user {
        let Ok(mut rule) = rules::validate(text) else {
            continue;
        };
        let mut text = text.trim().to_owned();
        let to_main = rule.target == rules::CURRENT_PROXY
            || (rule.target == rules::PROXY && !targets.contains(rules::PROXY));
        if rule.kind != "SUB-RULE" && to_main {
            let Some(group) = &main_group else {
                if seen.insert("策略组".into()) {
                    missing.push("策略组".into());
                }
                continue;
            };
            let mut fields = rules::split(&text);
            fields[2] = group.as_str();
            text = fields.join(",");
            rule.target = group.clone();
        }
        let absent = if rule.kind == "SUB-RULE" {
            (!sub_rules.contains(&rule.target)).then(|| format!("子规则 {}", rule.target))
        } else if !targets.contains(&rule.target) {
            Some(format!("策略 {}", rule.target))
        } else if rule.kind == "RULE-SET" && !providers.contains(&rule.payload) {
            Some(format!("规则集 {}", rule.payload))
        } else {
            None
        };
        if let Some(name) = absent {
            if seen.insert(name.clone()) {
                missing.push(name);
            }
            continue;
        }
        process |= rule.uses_process();
        kept.push(Value::from(text.as_str()));
    }
    let skipped = user.len() - kept.len();
    if skipped > 0 {
        notes.push(format!(
            "跳过 {skipped} 条附加规则：当前配置中不存在{}",
            missing.join("、")
        ));
    }
    if kept.is_empty() {
        return;
    }
    if process && map.get("find-process-mode").and_then(Value::as_str) == Some("off") {
        map.insert("find-process-mode".into(), Value::from("strict"));
        notes.push("附加规则含进程匹配，已将 find-process-mode 由 off 改为 strict".into());
    }
    let count = kept.len();
    if let Some(existing) = map.get("rules").and_then(Value::as_sequence) {
        kept.extend(existing.iter().cloned());
    }
    map.insert("rules".into(), Value::Sequence(kept));
    notes.push(format!("已在订阅规则前插入 {count} 条附加规则"));
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
