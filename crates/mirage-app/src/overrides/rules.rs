//! Extra rules (iteration 2 §3): `overrides/rules.yaml`, prepended to the
//! subscription's rules. Structure is checked here; regexes, geodata names
//! and the like are left to `mihomo -t`.
use crate::domain::error::{AppError, AppResult};
use crate::storage::{atomic, layout::Layout};
use serde_yaml_ng::Value;
use std::net::IpAddr;

const STAGE: &str = "overrides.rules";
const MAX_RULES: usize = 2000;
const MAX_RULE_LEN: usize = 1024;
const MAX_ERRORS: usize = 50;

/// Types offered by the visual editor, in drop-down order (§3.2).
pub const VISUAL_TYPES: [&str; 16] = [
    "DOMAIN",
    "DOMAIN-SUFFIX",
    "DOMAIN-KEYWORD",
    "DOMAIN-WILDCARD",
    "DOMAIN-REGEX",
    "GEOSITE",
    "IP-CIDR",
    "IP-CIDR6",
    "IP-ASN",
    "GEOIP",
    "SRC-IP-CIDR",
    "DST-PORT",
    "SRC-PORT",
    "PROCESS-NAME",
    "PROCESS-PATH",
    "NETWORK",
];

/// Other Mihomo rule types, accepted in YAML mode only.
const ADVANCED_TYPES: &[&str] = &[
    "IP-SUFFIX",
    "SRC-GEOIP",
    "SRC-IP-ASN",
    "SRC-IP-SUFFIX",
    "IN-PORT",
    "IN-TYPE",
    "IN-USER",
    "IN-NAME",
    "PROCESS-PATH-WILDCARD",
    "PROCESS-PATH-REGEX",
    "PROCESS-NAME-WILDCARD",
    "PROCESS-NAME-REGEX",
    "UID",
    "DSCP",
    "RULE-SET",
    "SUB-RULE",
    "AND",
    "OR",
    "NOT",
];

/// Policies every config has.
pub const BUILTIN_TARGETS: [&str; 4] = ["DIRECT", "REJECT", "REJECT-DROP", "PASS"];

const LOGICAL: [&str; 3] = ["AND", "OR", "NOT"];
const PARAMS: [&str; 2] = ["no-resolve", "src"];

/// Saved YAML (comments kept) and its validated rule strings.
#[derive(Debug, Default)]
pub struct Rules {
    pub text: String,
    pub rules: Vec<String>,
    /// Errors of a hand-edited file; such a file applies no rules.
    pub errors: Vec<String>,
}

impl Rules {
    pub fn load(layout: &Layout) -> AppResult<Self> {
        let text = match std::fs::read_to_string(layout.rules_file()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(AppError::storage(STAGE, e)),
        };
        Ok(match parse_yaml(&text) {
            Ok(rules) => Self {
                text,
                rules,
                errors: Vec::new(),
            },
            Err(errors) => Self {
                text,
                rules: Vec::new(),
                errors,
            },
        })
    }

    pub fn save(layout: &Layout, text: &str) -> AppResult<()> {
        atomic::write(&layout.rules_file(), text.as_bytes())
            .map_err(|e| AppError::storage(STAGE, e))
    }
}

/// A rule split at top-level commas: `TYPE,payload,target[,params]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub kind: String,
    pub payload: String,
    pub target: String,
    pub params: Vec<String>,
}

impl Rule {
    pub fn no_resolve(&self) -> bool {
        self.params.iter().any(|p| p == "no-resolve")
    }

    /// Editable in the visual list: a basic type with at most `no-resolve`.
    pub fn visual_index(&self) -> Option<usize> {
        if self.params.iter().any(|p| p != "no-resolve") {
            return None;
        }
        VISUAL_TYPES.iter().position(|t| *t == self.kind)
    }

    pub fn uses_process(&self) -> bool {
        self.kind.starts_with("PROCESS-")
            || (LOGICAL.contains(&self.kind.as_str()) && self.payload.contains("PROCESS-"))
    }
}

/// Splits on commas outside parentheses, trimming each field.
pub fn split(rule: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in rule.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                fields.push(rule[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    fields.push(rule[start..].trim());
    fields
}

/// Structural check of one rule (§3.4).
pub fn validate(rule: &str) -> Result<Rule, String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Err("规则为空".into());
    }
    if rule.len() > MAX_RULE_LEN {
        return Err(format!("超过 {MAX_RULE_LEN} 个字符"));
    }
    if !balanced(rule) {
        return Err("括号不配对".into());
    }
    let fields = split(rule);
    let kind = fields[0].to_ascii_uppercase();
    if kind == "MATCH" {
        return Err("不允许 MATCH：它会让所有订阅规则失效".into());
    }
    if !VISUAL_TYPES.contains(&kind.as_str()) && !ADVANCED_TYPES.contains(&kind.as_str()) {
        return Err(format!("未知的规则类型 {}", fields[0]));
    }
    if fields.len() < 3 {
        return Err("格式应为 类型,内容,目标".into());
    }
    let (payload, target) = (fields[1], fields[2]);
    if payload.is_empty() || target.is_empty() {
        return Err("内容和目标都不能为空".into());
    }
    let params: Vec<String> = fields[3..].iter().map(|p| p.to_string()).collect();
    if let Some(p) = params.iter().find(|p| !PARAMS.contains(&p.as_str())) {
        return Err(format!("不支持的参数 {p}，只允许 no-resolve、src"));
    }
    check_payload(&kind, payload)?;
    Ok(Rule {
        kind,
        payload: payload.to_owned(),
        target: target.to_owned(),
        params,
    })
}

fn balanced(rule: &str) -> bool {
    let mut depth = 0i32;
    for c in rule.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

fn check_payload(kind: &str, payload: &str) -> Result<(), String> {
    let ok = match kind {
        "AND" | "OR" | "NOT" | "SUB-RULE" => payload.starts_with('(') && payload.ends_with(')'),
        "IP-CIDR" | "IP-CIDR6" | "SRC-IP-CIDR" => parse_cidr(payload).is_some(),
        "IP-ASN" | "SRC-IP-ASN" => payload.parse::<u32>().is_ok(),
        "DST-PORT" | "SRC-PORT" | "IN-PORT" => valid_ports(payload),
        "NETWORK" => matches!(payload.to_ascii_lowercase().as_str(), "tcp" | "udp"),
        _ => !payload.chars().any(char::is_whitespace) || kind.starts_with("PROCESS-"),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{kind} 的内容“{payload}”格式不正确"))
    }
}

fn parse_cidr(text: &str) -> Option<(IpAddr, u8)> {
    let (ip, prefix) = text.split_once('/')?;
    let ip: IpAddr = ip.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    (prefix <= max).then_some((ip, prefix))
}

/// `443`, `8000-9000`, `80/443/8000-9000`.
fn valid_ports(text: &str) -> bool {
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p > 0);
    text.split('/').all(|part| match part.split_once('-') {
        Some((a, b)) => matches!((port(a), port(b)), (Some(a), Some(b)) if a <= b),
        None => port(part).is_some(),
    })
}

/// Builds a rule from the visual editor, normalising bare IPs to a host
/// prefix and picking IP-CIDR / IP-CIDR6 by address family.
pub fn build(
    kind_index: usize,
    payload: &str,
    target: &str,
    no_resolve: bool,
) -> Result<String, String> {
    let mut kind = *VISUAL_TYPES.get(kind_index).ok_or("请选择规则类型")?;
    let target = target.trim();
    if target.is_empty() || target.contains(',') {
        return Err("目标不能为空，也不能包含逗号".into());
    }
    let mut payload = payload.trim().to_owned();
    if payload.is_empty() || payload.contains(',') {
        return Err("匹配内容不能为空，也不能包含逗号".into());
    }
    if matches!(kind, "IP-CIDR" | "IP-CIDR6" | "SRC-IP-CIDR") {
        if let Ok(ip) = payload.parse::<IpAddr>() {
            payload = format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 });
        }
        if kind != "SRC-IP-CIDR"
            && let Some((ip, _)) = parse_cidr(&payload)
        {
            kind = if ip.is_ipv4() { "IP-CIDR" } else { "IP-CIDR6" };
        }
    }
    let resolve_param = no_resolve && matches!(kind, "IP-CIDR" | "IP-CIDR6" | "IP-ASN" | "GEOIP");
    let rule = format!(
        "{kind},{payload},{target}{}",
        if resolve_param { ",no-resolve" } else { "" }
    );
    validate(&rule).map(|_| rule)
}

/// `rules:` sequence of strings; empty text or only comments is an empty list.
pub fn parse_yaml(text: &str) -> Result<Vec<String>, Vec<String>> {
    let root: Value =
        serde_yaml_ng::from_str(text).map_err(|e| vec![format!("YAML 格式错误：{e}")])?;
    let items = match &root {
        Value::Null => return Ok(Vec::new()),
        Value::Mapping(map) => match map.get("rules") {
            None | Some(Value::Null) => return Ok(Vec::new()),
            Some(Value::Sequence(items)) if map.len() == 1 => items,
            Some(Value::Sequence(_)) => return Err(vec!["顶层只能有 rules 一个键".into()]),
            Some(_) => return Err(vec!["rules 必须是列表".into()]),
        },
        _ => return Err(vec!["顶层应为 rules: 列表".into()]),
    };
    if items.len() > MAX_RULES {
        return Err(vec![format!(
            "最多 {MAX_RULES} 条，当前 {} 条",
            items.len()
        )]);
    }
    let mut rules = Vec::with_capacity(items.len());
    let mut errors = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let no = i + 1;
        match item.as_str().map(validate) {
            Some(Ok(_)) => rules.push(item.as_str().unwrap_or_default().trim().to_owned()),
            Some(Err(e)) => errors.push(format!("第 {no} 条：{e}")),
            None => errors.push(format!("第 {no} 条：应为字符串")),
        }
    }
    if errors.is_empty() {
        return Ok(rules);
    }
    if errors.len() > MAX_ERRORS {
        let more = errors.len() - MAX_ERRORS;
        errors.truncate(MAX_ERRORS);
        errors.push(format!("另有 {more} 处错误未列出"));
    }
    Err(errors)
}

/// Serialises a list; comments of the previous text are not kept.
pub fn to_yaml(rules: &[String]) -> String {
    if rules.is_empty() {
        return "rules: []\n".into();
    }
    let mut map = serde_yaml_ng::Mapping::new();
    map.insert(
        "rules".into(),
        Value::Sequence(rules.iter().map(|r| Value::from(r.as_str())).collect()),
    );
    serde_yaml_ng::to_string(&map).unwrap_or_default()
}
