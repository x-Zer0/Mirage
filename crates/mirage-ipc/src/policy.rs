//! Managed-field overlay and provider isolation (DESIGN.md §9.3–§9.6).
//!
//! This is the single implementation of the policy. The app calls it when it
//! composes a run configuration; the SYSTEM service calls it again on whatever
//! YAML arrives over the pipe, so a compromised or buggy UI cannot smuggle in
//! an extra listener, an unrequested LAN-bound socket or a local-file provider.

use serde_yaml_ng::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;

/// Bump when the rules below change; Valid revisions are re-composed (§9.3).
/// v2: `tun` can be enabled and a minimal `dns` block may be injected.
/// v3: explicit user opt-in may expose only the mixed proxy listener to the LAN.
pub const OVERLAY_VERSION: u32 = 3;

/// TUN stack; the exact value is a M7 acceptance item (§9.6, §22).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TunStack {
    System,
    Gvisor,
    #[default]
    Mixed,
}

impl TunStack {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Gvisor => "gvisor",
            Self::Mixed => "mixed",
        }
    }
}

/// Present only when the user turned TUN on (§9.6); absent means
/// `tun.enable = false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TunOverlay {
    pub stack: TunStack,
}

/// DNS injected when the subscription has none enabled (§9.6). Values are the
/// 【待核验】 defaults from §22 pending M7 measurements.
pub const DEFAULT_NAMESERVERS: [&str; 2] = [
    "https://doh.pub/dns-query",
    "https://dns.alidns.com/dns-query",
];
pub const FAKE_IP_LISTEN: &str = "127.0.0.1:1053";

pub struct ManagedOverlay<'a> {
    pub mixed_port: u16,
    /// Application preference, never read from subscription YAML.
    pub allow_lan: bool,
    pub controller: SocketAddr,
    pub secret: &'a str,
    pub log_level: &'a str,
    /// Only set when the user picked a mode; otherwise the config's own `mode` stays.
    pub mode: Option<&'a str>,
    /// M7: `Some` enables TUN.
    pub tun: Option<TunOverlay>,
    /// Providers are confined below `./providers/<profile_id>/` (§9.4).
    pub profile_id: &'a str,
}

/// Keys deleted outright: extra listeners, subscription access rules, controller side doors.
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

/// Parses YAML (expanding `<<` merge keys), applies the overlay and returns
/// the serialised runtime document.
pub fn apply(text: &str, o: &ManagedOverlay<'_>) -> Result<(String, Vec<String>), String> {
    if o.profile_id.is_empty()
        || o.profile_id.len() > 64
        || !o
            .profile_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("配置 ID 不合法".into());
    }
    let mut root = parse(text)?;
    let map = root
        .as_mapping_mut()
        .ok_or_else(|| "配置顶层不是映射".to_owned())?;

    let mut notes = Vec::new();
    for key in REMOVED_KEYS {
        if map.remove(*key).is_some() {
            notes.push(format!("已移除订阅中的 {key} 设置"));
        }
    }
    override_managed(map, o, &mut notes);
    isolate_providers(map, "proxy-providers", "proxy", o.profile_id, &mut notes)?;
    isolate_providers(map, "rule-providers", "rule", o.profile_id, &mut notes)?;
    enforce_provider_paths(map)?;
    // Bind only the DNS listener; keep upstreams, filters and other DNS policy.
    if let Some(dns) = map.get_mut("dns").and_then(Value::as_mapping_mut)
        && let Some(listen) = dns.get("listen").and_then(Value::as_str)
        && !listen.is_empty()
    {
        let port = listen
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p != 0)
            .ok_or("DNS 监听端口不合法")?;
        let local = format!("127.0.0.1:{port}");
        if listen != local {
            dns.insert("listen".into(), Value::from(local));
            notes.push("已将 DNS 监听地址限制为环回地址".into());
        }
    }

    let yaml = serde_yaml_ng::to_string(&root).map_err(|e| e.to_string())?;
    Ok((yaml, notes))
}

/// Parses and expands `<<` merge keys so they survive re-serialisation (§9.1).
pub fn parse(text: &str) -> Result<Value, String> {
    let mut root: Value =
        serde_yaml_ng::from_str(text).map_err(|e| format!("YAML 解析失败：{e}"))?;
    root.apply_merge()
        .map_err(|e| format!("YAML 合并键无效：{e}"))?;
    Ok(root)
}

fn override_managed(map: &mut Mapping, o: &ManagedOverlay<'_>, notes: &mut Vec<String>) {
    set_key(map, notes, "mixed-port", Value::from(o.mixed_port));
    set_key(map, notes, "allow-lan", Value::Bool(o.allow_lan));
    set_key(
        map,
        notes,
        "bind-address",
        Value::from(if o.allow_lan { "*" } else { "127.0.0.1" }),
    );
    set_key(
        map,
        notes,
        "external-controller",
        Value::from(o.controller.to_string()),
    );
    set_key(map, notes, "secret", Value::from(o.secret));
    set_key(map, notes, "log-level", Value::from(o.log_level));
    set_key(map, notes, "geo-auto-update", Value::Bool(false));
    set_key(map, notes, "tun", tun_value(o.tun));
    if o.tun.is_some() {
        ensure_dns(map, notes);
    }
    if let Some(mode) = o.mode {
        set_key(map, notes, "mode", Value::from(mode));
    }
    set_key(
        map,
        notes,
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

/// Writes one managed key and notes security-relevant overrides; a no-op when
/// the subscription already carried the same value.
fn set_key(map: &mut Mapping, notes: &mut Vec<String>, key: &str, value: Value) {
    if let Some(old) = map.insert(key.into(), value.clone())
        && old != value
        && is_security_key(key)
    {
        notes.push(format!("已覆盖订阅中的 {key} 设置"));
    }
}

/// §9.3: `tun` is replaced wholesale; §9.6 defines the enabled shape.
fn tun_value(tun: Option<TunOverlay>) -> Value {
    let Some(tun) = tun else {
        return mapping([("enable", Value::Bool(false))]);
    };
    mapping([
        ("enable", Value::Bool(true)),
        ("stack", Value::from(tun.stack.as_str())),
        ("auto-route", Value::Bool(true)),
        ("auto-detect-interface", Value::Bool(true)),
        ("dns-hijack", Value::Sequence(vec![Value::from("any:53")])),
        ("strict-route", Value::Bool(false)),
    ])
}

/// §9.6: only fills in what the subscription does not provide; an existing
/// DNS policy is never rewritten.
fn ensure_dns(map: &mut Mapping, notes: &mut Vec<String>) {
    if map
        .get("dns")
        .and_then(|v| v.get("enable"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        map.insert("dns".into(), minimal_dns());
        notes.push("订阅未启用 DNS，已注入最小 DNS 配置以配合 TUN".into());
    }
}
fn minimal_dns() -> Value {
    Value::Mapping(
        minimal_dns_entries()
            .into_iter()
            .map(|(k, v)| (Value::from(k), v))
            .collect(),
    )
}

fn minimal_dns_entries() -> Vec<(&'static str, Value)> {
    vec![
        ("enable", Value::Bool(true)),
        ("enhanced-mode", Value::from("fake-ip")),
        ("listen", Value::from(FAKE_IP_LISTEN)),
        (
            "nameserver",
            Value::Sequence(
                DEFAULT_NAMESERVERS
                    .iter()
                    .map(|s| Value::from(*s))
                    .collect(),
            ),
        ),
    ]
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
    profile_id: &str,
    notes: &mut Vec<String>,
) -> Result<(), String> {
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
                return Err(format!("{section}.{name} 引用本地文件，不支持"));
            }
            Some("inline") => {
                p.remove("path");
            }
            _ => return Err(format!("{section}.{name} 的 provider 类型不支持")),
        }
    }
    if !providers.is_empty() {
        notes.push(format!("已将 {section} 的缓存路径隔离到当前配置"));
    }
    Ok(())
}

/// Defense in depth for the service (§11.7): every provider `path` that is
/// present must be relative to the working directory and stay inside it, so a
/// config cannot read or overwrite arbitrary SYSTEM-readable files.
fn enforce_provider_paths(map: &Mapping) -> Result<(), String> {
    for section in ["proxy-providers", "rule-providers"] {
        let Some(providers) = map.get(section).and_then(Value::as_mapping) else {
            continue;
        };
        for (name, provider) in providers {
            let Some(path) = provider.get("path").and_then(Value::as_str) else {
                continue;
            };
            let name = name.as_str().unwrap_or_default();
            if !safe_relative_path(path) {
                return Err(format!(
                    "{section}.{name} 的缓存路径必须是工作目录下的相对路径"
                ));
            }
        }
    }
    Ok(())
}

fn safe_relative_path(path: &str) -> bool {
    let normalised = path.replace('\\', "/");
    !normalised.starts_with('/')
        && !normalised.contains(':')
        && normalised
            .split('/')
            .all(|segment| segment != ".." && segment != "~")
}

fn mapping<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Mapping(
        pairs
            .into_iter()
            .map(|(k, v)| (Value::from(k), v))
            .collect(),
    )
}
