//! Converts stored profiles/revisions into application list snapshots.
use super::{state::ProfileView, updates};
use crate::domain::model::{EgressPolicy, Profile, SubscriptionSource, Validity};
use crate::subscriptions::revisions;

pub(super) fn profile_view(
    layout: &crate::storage::layout::Layout,
    p: &Profile,
    selected: &str,
    updating: bool,
) -> ProfileView {
    let active = p
        .active_revision
        .as_ref()
        .and_then(|rev| revisions::find(layout, p, rev).ok().flatten());
    let detail = active
        .as_ref()
        .map(|r| match &r.validity {
            Validity::Valid => format!(
                "{} 个节点 · {} 个组 · {} 条规则",
                r.summary.proxies, r.summary.groups, r.summary.rules
            ),
            Validity::Invalid(reason) => format!("无效：{reason}"),
            Validity::Unchecked => "未校验".into(),
        })
        .unwrap_or_else(|| "没有可用版本".into());
    let source = match &p.source {
        SubscriptionSource::RemoteUrl { host, .. } => format!("订阅 {host}"),
        SubscriptionSource::ProviderAccount { provider_id, .. } => {
            format!("{}账号", crate::providers::account_name(provider_id))
        }
        other => other.label().into(),
    };
    ProfileView {
        id: p.id.clone(),
        name: p.name.clone(),
        source,
        account_id: super::accounts::account_of(p)
            .unwrap_or_default()
            .to_owned(),
        detail,
        updated: updates::profile_time(p.updated_at),
        selected: p.id == selected,
        remote: p.is_remote(),
        updating,
        usage: p.usage.map(usage_text).unwrap_or_default(),
        fetch_error: p
            .last_fetch
            .as_ref()
            .and_then(|f| f.error.clone())
            .unwrap_or_default(),
        has_url: matches!(p.source, SubscriptionSource::RemoteUrl { .. }),
        interval_hours: p.update_policy.interval_hours,
        egress: EgressPolicy::ALL
            .iter()
            .position(|e| *e == p.update_policy.egress)
            .unwrap_or(0),
        policy_notes: active.map(|r| r.policy_notes).unwrap_or_default(),
    }
}

/// "已用 1.2 GiB / 100.0 GiB · 到期 2026-12-01"; expiry 0/missing = 未提供 (§15).
fn usage_text(u: crate::domain::model::UsageInfo) -> String {
    let used = gib(u.upload.saturating_add(u.download));
    let total = if u.total > 0 {
        gib(u.total)
    } else {
        "不限".into()
    };
    let expire = u
        .expire
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "未提供".into());
    format!("已用 {used} / {total} · 到期 {expire}")
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}
