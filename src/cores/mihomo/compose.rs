//! Managed-field overlay and policy filter (DESIGN.md §9). Only top-level keys
//! and provider `path` fields are touched; everything else is passed through.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{ConfigSummary, ProfileId, RoutingMode};
use serde_yaml_ng::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;

const STAGE: &str = "mihomo.compose";
/// Bump when the rules below change; Valid revisions are re-composed (§9.3).
pub const OVERLAY_VERSION: u32 = 1;

pub struct ManagedOverlay<'a> {
    pub mixed_port: u16,
    pub controller: SocketAddr,
    pub secret: &'a str,
    pub log_level: &'a str,
    pub profile_id: &'a ProfileId,
    /// Only set when the user picked a mode; otherwise the config's own `mode` stays.
    pub mode: Option<RoutingMode>,
}

pub struct Composed {
    pub yaml: String,
    pub summary: ConfigSummary,
    pub policy_notes: Vec<String>,
}

/// Keys deleted outright: extra listeners, LAN exposure, controller side doors.
const REMOVED_KEYS: &[&str] = &[
    "port",
    "socks-port",
    "redir-port",
    "tproxy-port",
    "lan-allowed-ips",
    "lan-disallowed-ips",
    "authentication",
    "skip-auth-prefixes",
    "external-controller-tls",
    "external-controller-unix",
    "external-controller-pipe",
    "external-controller-cors",
    "external-doh-server",
    "tls",
    "external-ui",
    "external-ui-url",
    "external-ui-name",
    "listeners",
    "tunnels",
    "iptables",
    "ebpf",
    "auto-redirect",
    "geox-url",
];

/// Parses `text`, applies the overlay and serialises the run configuration.
pub fn compose(text: &str, overlay: &ManagedOverlay<'_>) -> AppResult<Composed> {
    let mut root = parse(text)?;
    let before = summarize(&root);
    let map = root
        .as_mapping_mut()
        .ok_or_else(|| fail(AppErrorKind::NotAConfig, "配置顶层不是映射"))?;

    let mut notes = Vec::new();
    for key in REMOVED_KEYS {
        if map.remove(*key).is_some() {
            notes.push(format!("已移除订阅中的 {key} 设置"));
        }
    }
    override_managed(map, overlay, &mut notes);
    isolate_providers(
        map,
        "proxy-providers",
        "proxy",
        overlay.profile_id,
        &mut notes,
    )?;
    isolate_providers(
        map,
        "rule-providers",
        "rule",
        overlay.profile_id,
        &mut notes,
    )?;

    let summary = summarize(&root);
    if summary != before {
        return Err(AppError::internal(
            STAGE,
            format!("合成前后摘要不一致：{before:?} → {summary:?}"),
        ));
    }
    let yaml = serde_yaml_ng::to_string(&root).map_err(|e| AppError::internal(STAGE, e))?;
    Ok(Composed {
        yaml,
        summary,
        policy_notes: notes,
    })
}

/// Parses and expands `<<` merge keys so they survive re-serialisation (§9.1).
pub fn parse(text: &str) -> AppResult<Value> {
    let mut root: Value = serde_yaml_ng::from_str(text)
        .map_err(|e| fail(AppErrorKind::NotAConfig, &format!("YAML 解析失败：{e}")))?;
    root.apply_merge()
        .map_err(|e| fail(AppErrorKind::NotAConfig, &format!("YAML 合并键无效：{e}")))?;
    Ok(root)
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

fn override_managed(map: &mut Mapping, o: &ManagedOverlay<'_>, notes: &mut Vec<String>) {
    let mut set = |key: &str, value: Value| {
        if let Some(old) = map.insert(key.into(), value.clone())
            && old != value
            && is_security_key(key)
        {
            notes.push(format!("已覆盖订阅中的 {key} 设置"));
        }
    };
    set("mixed-port", Value::from(o.mixed_port));
    set("allow-lan", Value::Bool(false));
    set("bind-address", Value::from("127.0.0.1"));
    set("external-controller", Value::from(o.controller.to_string()));
    set("secret", Value::from(o.secret));
    set("log-level", Value::from(o.log_level));
    set("geo-auto-update", Value::Bool(false));
    set("tun", mapping([("enable", Value::Bool(false))]));
    if let Some(mode) = o.mode {
        set("mode", Value::from(mode.as_str()));
    }
    set(
        "profile",
        mapping([
            ("store-selected", Value::Bool(false)),
            ("store-fake-ip", Value::Bool(true)),
        ]),
    );
    if let Some(ntp) = map.get_mut("ntp").and_then(Value::as_mapping_mut)
        && ntp.contains_key("write-to-system")
    {
        ntp.insert("write-to-system".into(), Value::Bool(false));
    }
}

fn is_security_key(key: &str) -> bool {
    matches!(
        key,
        "allow-lan" | "bind-address" | "external-controller" | "secret" | "tun"
    )
}

/// Rewrites http provider paths under `providers/<profile>/`, rejects file providers (§9.4).
fn isolate_providers(
    map: &mut Mapping,
    section: &str,
    kind: &str,
    profile_id: &ProfileId,
    notes: &mut Vec<String>,
) -> AppResult<()> {
    let Some(providers) = map.get_mut(section).and_then(Value::as_mapping_mut) else {
        return Ok(());
    };
    for (name, provider) in providers.iter_mut() {
        let name = name.as_str().unwrap_or_default().to_owned();
        let Some(p) = provider.as_mapping_mut() else {
            continue;
        };
        match p.get("type").and_then(Value::as_str) {
            Some("http") => {
                let digest = hex::encode(Sha256::digest(name.as_bytes()));
                let path = format!("./providers/{profile_id}/{kind}-{}.yaml", &digest[..12]);
                p.insert("path".into(), Value::from(path));
            }
            Some("file") => {
                return Err(fail(
                    AppErrorKind::PolicyRejected,
                    &format!("{section}.{name} 引用本地文件，不支持"),
                ));
            }
            _ => {}
        }
    }
    if !providers.is_empty() {
        notes.push(format!("已将 {section} 的缓存路径隔离到当前配置"));
    }
    Ok(())
}

fn mapping<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Mapping(
        pairs
            .into_iter()
            .map(|(k, v)| (Value::from(k), v))
            .collect(),
    )
}

fn fail(kind: AppErrorKind, detail: &str) -> AppError {
    AppError::new(kind, STAGE, detail)
}
