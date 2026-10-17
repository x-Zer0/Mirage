//! Converts [`AppStateView`] snapshots into Slint properties.
//!
//! Every list model is only replaced when its rows actually changed, so a
//! snapshot that differs in one field does not rebuild the node or profile
//! lists (and the views do not lose scroll position or re-layout).

use super::{AccountRow, AppTray, GroupRow, MainWindow, NodeRow, ProfileRow, TrayState};
use crate::app::state::{AppStateView, Connectivity, RuntimeStatus, TrafficSample};
use crate::app::status_text;
use crate::cores::mihomo::controller::{GLOBAL_GROUP, NodeView, ProxyGroupView};
use crate::domain::model::{EgressPolicy, RoutingMode};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::collections::VecDeque;
use std::rc::Rc;

const SLOW_MS: u32 = 500;
/// §17.4: at most this many nodes per group are listed; the rest are found
/// through the search box.
const MAX_LISTED_NODES: usize = 1000;
/// Seconds shown in the traffic chart.
pub const HISTORY: usize = 60;

/// Previous snapshot, to skip unchanged models.
#[derive(Default)]
pub struct Applied {
    last: Option<std::sync::Arc<AppStateView>>,
    pub list: ListOptions,
}

/// UI-only node list preferences (§17.1); never sent to the core.
#[derive(Default)]
pub struct ListOptions {
    /// Nodes ordered by measured delay.
    pub sort_by_delay: bool,
    /// Lower-cased search text; empty = no filter.
    pub filter: String,
}

impl Applied {
    /// Rebuilds the group model after the sort or the search changed.
    pub fn relist(&self, w: &MainWindow) {
        if let Some(s) = &self.last {
            w.set_groups(rc(shown_groups(s)
                .into_iter()
                .map(|g| group_row(g, &self.list))));
        }
    }
}

pub fn apply(w: &MainWindow, s: &std::sync::Arc<AppStateView>, applied: &mut Applied) {
    let prev = applied.last.as_deref();
    let changed = |f: &dyn Fn(&AppStateView) -> bool| prev.is_none_or(f);

    w.set_core_version(s.core_version.clone().into());
    w.set_status_text(status_text(&s.runtime).into());
    w.set_running(s.runtime.is_up());
    w.set_failed(matches!(s.runtime, RuntimeStatus::Failed(_)));
    let busy = s.busy.is_some()
        || s.service_busy
        || matches!(
            s.runtime,
            RuntimeStatus::Starting
                | RuntimeStatus::Stopping
                | RuntimeStatus::Reloading
                | RuntimeStatus::Restarting
        );
    w.set_busy(busy);
    w.set_busy_text(
        s.busy
            .clone()
            .or_else(|| busy.then(|| status_text(&s.runtime)))
            .unwrap_or_default()
            .into(),
    );
    w.set_mixed_port(i32::from(s.mixed_port));
    w.set_system_proxy_wanted(s.system_proxy_wanted);
    w.set_system_proxy_active(s.system_proxy_active);
    w.set_tun_wanted(s.tun_wanted);
    w.set_tun_active(s.tun_active);
    w.set_service_installed(s.service_installed);
    w.set_service_busy(s.service_busy);
    let (text, level) = connectivity_text(s);
    w.set_connectivity_text(text.into());
    w.set_connectivity_level(level);
    w.set_routing_mode(mode_index(s.routing_mode));
    w.set_exit_node(exit_node(s).into());
    let selected = s.profiles.iter().find(|p| p.selected);
    w.set_profile_name(selected.map(|p| p.name.clone()).unwrap_or_default().into());
    // Plan and usage of the account behind the active profile, if any.
    let account = selected.and_then(|p| s.accounts.iter().find(|a| a.profile_id == p.id));
    w.set_account_plan(account.map(|a| a.plan.clone()).unwrap_or_default().into());
    w.set_account_usage(selected.map(|p| p.usage.clone()).unwrap_or_default().into());

    if changed(&|p| p.profiles != s.profiles) {
        w.set_profiles(rc(s.profiles.iter().map(|p| ProfileRow {
            id: p.id.clone().into(),
            name: p.name.clone().into(),
            source: p.source.clone().into(),
            account_id: p.account_id.clone().into(),
            detail: p.detail.clone().into(),
            updated: p.updated.clone().into(),
            selected: p.selected,
            remote: p.remote,
            updating: p.updating,
            usage: p.usage.clone().into(),
            fetch_error: p.fetch_error.clone().into(),
            has_url: p.has_url,
            interval_hours: i32::try_from(p.interval_hours).unwrap_or(i32::MAX),
            egress: i32::try_from(p.egress).unwrap_or(0),
            policy_notes: rc(p.policy_notes.iter().map(SharedString::from)),
        })));
    }
    if changed(&|p| p.groups != s.groups || p.routing_mode != s.routing_mode) {
        update_groups(w, &shown_groups(s), &applied.list);
    }
    w.set_testing_group(s.testing_group.clone().unwrap_or_default().into());
    if changed(&|p| p.policy_notes != s.policy_notes) {
        w.set_policy_notes(rc(s.policy_notes.iter().map(SharedString::from)));
    }
    if changed(&|p| p.check_lines != s.check_lines) {
        w.set_check_lines(rc(s.check_lines.iter().map(SharedString::from)));
    }
    let error = s.error.as_ref();
    w.set_error_stage(error.map(|e| e.stage.clone()).unwrap_or_default().into());
    w.set_error_text(error.map(|e| e.text.clone()).unwrap_or_default().into());
    w.set_error_retry(error.is_some_and(|e| e.retry.is_some()));
    if changed(&|p| p.accounts != s.accounts) {
        w.set_accounts(rc(s.accounts.iter().map(|a| AccountRow {
            id: a.id.clone().into(),
            name: a.name.clone().into(),
            provider_name: a.provider_name.clone().into(),
            status: a.status.clone().into(),
            status_level: a.status_level,
            busy: a.busy,
            plan: a.plan.clone().into(),
            usage: a.usage.clone().into(),
            expires: a.expires.clone().into(),
        })));
    }
    if prev.is_some_and(|p| p.login_success != s.login_success) {
        w.invoke_close_account_login();
    }
    if changed(&|p| p.revealed_account != s.revealed_account) {
        w.set_revealed_account_id(s.revealed_account.as_ref().map_or("", |(id, _)| id).into());
        w.set_revealed_account_name(
            s.revealed_account
                .as_ref()
                .map_or("", |(_, name)| name.expose())
                .into(),
        );
    }
    w.set_login_busy(s.login_busy);
    w.set_login_message(s.login_message.clone().unwrap_or_default().into());
    w.set_probing_hosts(s.probing_hosts);
    if changed(&|p| p.provider_hosts != s.provider_hosts) {
        w.set_provider_hosts(rc(s.provider_hosts.iter().map(SharedString::from)));
    }
    if prev.is_none() {
        w.set_egress_options(rc(EgressPolicy::ALL
            .iter()
            .map(|e| SharedString::from(e.label()))));
    }
    let st = &s.settings;
    w.set_settings_port(i32::from(st.mixed_port));
    w.set_settings_bypass(st.bypass.clone().into());
    w.set_settings_user_agent(st.default_user_agent.clone().into());
    w.set_settings_provider_egress(i32::try_from(st.provider_egress).unwrap_or(0));
    w.set_settings_auto_connect(st.auto_connect);
    w.set_settings_close_to_tray(st.close_to_tray);
    w.set_settings_autostart(st.autostart);
    w.set_settings_tun_disable_system_proxy(st.tun_disable_system_proxy);
    w.set_core_path(st.core_path.clone().into());
    w.set_geo_updating(s.geo_updating);
    w.set_exporting(s.exporting);

    applied.last = Some(s.clone());
}

/// Replaces the group model only when the group list changed shape; delay
/// and selection changes are written row by row into the existing model.
fn update_groups(w: &MainWindow, groups: &[&ProxyGroupView], list: &ListOptions) {
    let current = w.get_groups();
    let same_groups = current.row_count() == groups.len()
        && groups.iter().enumerate().all(|(i, g)| {
            current
                .row_data(i)
                .is_some_and(|r| r.name == g.name.as_str())
        });
    if !same_groups {
        w.set_groups(rc(groups.iter().map(|g| group_row(g, list))));
        return;
    }
    for (i, g) in groups.iter().enumerate() {
        let row = group_row(g, list);
        if let Some(old) = current.row_data(i)
            && (old.now != row.now
                || old.hidden != row.hidden
                || !same_rows(&old.nodes, &row.nodes))
        {
            current.set_row_data(i, row);
        }
    }
}

fn same_rows(a: &ModelRc<NodeRow>, b: &ModelRc<NodeRow>) -> bool {
    a.row_count() == b.row_count() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
}

pub fn apply_tray(t: &AppTray, s: &AppStateView) {
    t.set_state(match s.runtime {
        RuntimeStatus::Running | RuntimeStatus::Reloading => TrayState::Connected,
        RuntimeStatus::Restarting => TrayState::Restarting,
        RuntimeStatus::Failed(_) => TrayState::Error,
        _ => TrayState::Idle,
    });
    let selected = s.profiles.iter().find(|p| p.selected);
    let detail = match (&s.runtime, selected) {
        (r, Some(p)) if r.is_up() => {
            let node = exit_node(s);
            if node.is_empty() {
                p.name.clone()
            } else {
                format!("{} · {node}", p.name)
            }
        }
        _ => String::new(),
    };
    t.set_detail(detail.into());
    t.set_system_proxy(s.system_proxy_wanted);
    t.set_tun_wanted(s.tun_wanted);
    t.set_running(s.runtime.is_up());
    t.set_busy(
        s.service_busy
            || matches!(
                s.runtime,
                RuntimeStatus::Starting
                    | RuntimeStatus::Stopping
                    | RuntimeStatus::Reloading
                    | RuntimeStatus::Restarting
            ),
    );
    t.set_routing_mode(mode_index(s.routing_mode));
}

pub fn mode_index(mode: Option<RoutingMode>) -> i32 {
    mode.and_then(|m| RoutingMode::ALL.iter().position(|x| *x == m))
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(-1)
}

/// Groups as listed on the nodes page. `GLOBAL` only matters in global
/// mode (all traffic goes through it), so it is listed first then and
/// hidden otherwise.
fn shown_groups(s: &AppStateView) -> Vec<&ProxyGroupView> {
    let (global, rest): (Vec<_>, Vec<_>) = s.groups.iter().partition(|g| g.name == GLOBAL_GROUP);
    if s.routing_mode == Some(RoutingMode::Global) {
        global.into_iter().chain(rest).collect()
    } else {
        rest
    }
}

/// The node most traffic leaves through: in rule mode the first group (the
/// main selector by convention), in global mode `GLOBAL`, followed down
/// nested groups to a leaf. Empty when unknown.
fn exit_node(s: &AppStateView) -> String {
    let start = match s.routing_mode {
        Some(RoutingMode::Direct) => return "直连".into(),
        Some(RoutingMode::Global) => s.groups.iter().find(|g| g.name == GLOBAL_GROUP),
        _ => s.groups.iter().find(|g| g.name != GLOBAL_GROUP),
    };
    let Some(start) = start else {
        return String::new();
    };
    let mut current = start.now.clone();
    for _ in 0..8 {
        match s.groups.iter().find(|g| g.name == current) {
            Some(g) if !g.now.is_empty() => current = g.now.clone(),
            _ => break,
        }
    }
    current
}

fn connectivity_text(s: &AppStateView) -> (String, i32) {
    if !s.runtime.is_up() {
        return ("未连接".into(), 0);
    }
    match &s.connectivity {
        Connectivity::Unknown => ("未检测".into(), 0),
        Connectivity::Checking => ("检测中…".into(), 0),
        Connectivity::Reachable(ms) => (format!("可访问互联网（{ms} ms）"), 1),
        Connectivity::Unreachable(reason) => (format!("无法访问互联网：{reason}"), 2),
    }
}

fn group_row(g: &ProxyGroupView, list: &ListOptions) -> GroupRow {
    let mut nodes: Vec<&NodeView> = g
        .nodes
        .iter()
        .filter(|n| list.filter.is_empty() || n.name.to_lowercase().contains(&list.filter))
        .collect();
    if list.sort_by_delay {
        // Measured first (fastest on top), then timeouts, then untested.
        nodes.sort_by_key(|n| match n.delay {
            Some(0) => (1, 0),
            Some(ms) => (0, ms),
            None => (2, 0),
        });
    }
    let hidden = g.nodes.len() - nodes.len().min(MAX_LISTED_NODES);
    nodes.truncate(MAX_LISTED_NODES);
    GroupRow {
        name: g.name.clone().into(),
        kind: g.kind.clone().into(),
        now: g.now.clone().into(),
        selectable: g.selectable,
        hidden: i32::try_from(hidden).unwrap_or(i32::MAX),
        nodes: rc(nodes.into_iter().map(|n| {
            let (delay, level) = match n.delay {
                None => (SharedString::new(), 0),
                Some(0) => ("失败".into(), 3),
                Some(ms) => (format!("{ms} ms").into(), if ms <= SLOW_MS { 1 } else { 2 }),
            };
            NodeRow {
                name: n.name.clone().into(),
                delay,
                delay_level: level,
                current: n.name == g.now,
            }
        })),
    }
}

/// Writes the rate labels and the sparkline paths (60 x 100 viewbox).
pub fn apply_traffic(w: &MainWindow, history: &VecDeque<TrafficSample>) {
    let last = history.back().copied().unwrap_or_default();
    w.set_up_rate(rate(last.up).into());
    w.set_down_rate(rate(last.down).into());
    let peak = history
        .iter()
        .map(|s| s.up.max(s.down))
        .max()
        .unwrap_or(0)
        .max(1024) as f64;
    w.set_up_path(path(history.iter().map(|s| s.up), peak).into());
    w.set_down_path(path(history.iter().map(|s| s.down), peak).into());
}

fn path(values: impl ExactSizeIterator<Item = u64>, peak: f64) -> String {
    let offset = HISTORY.saturating_sub(values.len());
    let mut out = String::new();
    for (i, v) in values.enumerate() {
        let x = offset + i;
        // Zero sits just above the clipped bottom edge so the line stays visible.
        let y = 98.0 - (v as f64 / peak * 94.0);
        out.push_str(&format!("{}{x} {y:.1} ", if i == 0 { "M " } else { "L " }));
    }
    out
}

pub fn rate(bytes_per_sec: u64) -> String {
    const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut v = bytes_per_sec as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes_per_sec} B/s")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

pub fn rc<T: Clone + 'static>(items: impl Iterator<Item = T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(items.collect::<Vec<_>>())))
}
