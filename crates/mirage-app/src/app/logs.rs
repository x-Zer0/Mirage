//! Session connection records and the bounded page sent to the UI.
use crate::domain::model::Timestamp;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionLog {
    pub id: u64,
    pub at: Timestamp,
    pub protocol: String,
    pub address: String,
    pub source: String,
    pub action: String,
    pub rule: String,
    pub profile: String,
    pub node: String,
    pub details: String,
}

#[derive(Debug, Clone)]
pub struct ParsedConnection {
    pub at: Timestamp,
    pub protocol: String,
    pub source: String,
    pub address: String,
    pub action: String,
    pub rule: String,
    pub route: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationLog {
    pub id: String,
    pub at: Timestamp,
    pub action: String,
    pub object: String,
    pub source: String,
    pub result: String,
    pub summary: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogsPage {
    pub revision: u64,
    pub connections: Arc<Vec<ConnectionLog>>,
    pub operations: Arc<Vec<OperationLog>>,
    pub total: usize,
    pub status: String,
}

#[derive(Debug, Default)]
pub struct LogQuery {
    pub active: bool,
    pub operations: bool,
    pub text: String,
    pub filter: usize,
    pub paused: bool,
    pub anchor: Option<u64>,
}

#[derive(Default)]
pub struct ConnectionHistory {
    pub rows: VecDeque<ConnectionLog>,
    next_id: u64,
    pub min_epoch: u64,
}
impl ConnectionHistory {
    pub fn push(&mut self, mut row: ConnectionLog, limit: usize) {
        if limit == 0 {
            return;
        }
        self.next_id += 1;
        row.id = self.next_id;
        if self.rows.len() >= limit {
            self.rows.pop_front();
        }
        self.rows.push_back(row);
    }

    pub fn set_limit(&mut self, limit: usize) {
        while self.rows.len() > limit {
            self.rows.pop_front();
        }
    }

    pub fn clear(&mut self, min_epoch: u64) {
        self.min_epoch = min_epoch;
        self.rows.clear();
    }
}

/// Each routing event is a connection. Do not collapse concurrent source ports
/// or later port reuse with a time-window heuristic.
pub fn parse_connection(payload: &str) -> Option<ParsedConnection> {
    let (protocol, body) = payload.strip_prefix('[')?.split_once(']')?;
    if !matches!(protocol, "TCP" | "UDP") {
        return None;
    }
    let (left, right) = body.trim().split_once(" --> ")?;
    let (address, tail) = right.split_once(' ')?;
    let source_split = left.rsplit_once(' ');
    let source = source_split.map_or_else(|| left.to_owned(), |(_, value)| value.to_owned());
    let left_head = source_split.map_or(left, |(head, _)| head).trim();
    let (action, rule, route, error) = if let Some(head) = left_head.strip_prefix("dial ") {
        let (route, rule) = head.split_once(" (match ")?;
        let error = tail.strip_prefix("error: ")?.to_owned();
        (
            "失败",
            rule.trim_end_matches(')').to_owned(),
            route.to_owned(),
            error,
        )
    } else {
        let (rule, route) = if let Some(route) = tail.strip_prefix("using ") {
            (
                if route == "DIRECT" {
                    "DIRECT"
                } else {
                    "Global"
                },
                route,
            )
        } else {
            let (rule, route) = tail.split_once(" using ")?;
            (rule.strip_prefix("match ").unwrap_or(rule), route)
        };
        let rule = rule.to_owned();
        let action = if route == "DIRECT" {
            "直连"
        } else if route == "REJECT" || route.starts_with("REJECT-") {
            "拦截"
        } else {
            "代理"
        };
        (action, rule, route.to_owned(), String::new())
    };
    Some(ParsedConnection {
        at: crate::domain::model::now(),
        protocol: protocol.into(),
        source,
        address: address.into(),
        action: action.into(),
        rule,
        route,
        error,
    })
}

pub fn friendly_rule(rule: &str) -> String {
    let rule = rule.trim_end_matches('/');
    let (kind, value) = rule
        .split_once('(')
        .map(|(k, v)| (k, v.trim_end_matches(')')))
        .or_else(|| rule.split_once('/'))
        .unwrap_or((rule, ""));
    let label = match kind {
        "Domain" => "域名",
        "DomainSuffix" => "域名后缀",
        "DomainKeyword" => "域名关键词",
        "IPCIDR" | "IPCIDR6" => "IP 网段",
        "GeoIP" => "IP 地区",
        "GeoSite" => "站点分类",
        "Match" | "MATCH" => "兜底规则",
        "RuleSet" => "规则集",
        "ProcessName" => "进程名",
        "GLOBAL" | "Global" => "全局模式",
        "DIRECT" => "直连模式",
        other => other,
    };
    if value.is_empty() {
        label.to_owned()
    } else {
        format!("{label} · {value}")
    }
}
