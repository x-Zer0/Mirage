//! Converts [`AppStateView`] snapshots into Slint properties.
//!
//! Every list model is only replaced when its rows actually changed, so a
//! snapshot that differs in one field does not rebuild the node or profile
//! lists (and the views do not lose scroll position or re-layout).

use super::{AccountRow, AppTray, GroupRow, MainWindow, NodeRow, ProfileRow, TrayState, flags};
use crate::app::state::{AppStateView, Connectivity, RuntimeStatus, TrafficSample};
use crate::app::status_text;
use crate::cores::mihomo::controller::{
    DelayOutcome, GLOBAL_GROUP, NodeTestState, NodeView, ProxyGroupView,
};
use crate::domain::model::{EgressPolicy, RoutingMode};
use slint::{Image, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
};

const SLOW_MS: u32 = 500;
/// §17.4: at most this many nodes per group are listed; the rest are found
/// through the search box.
const MAX_LISTED_NODES: usize = 1000;
/// Seconds shown in the traffic chart.
pub const HISTORY: usize = 60;
const TRAY_ICON_SVG: &str = include_str!("../../assets/icons/tray/connected.svg");

thread_local! {
    static TRAY_ICONS: RefCell<HashMap<(String, bool), Image>> = RefCell::new(HashMap::new());
}

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
    /// Full group order at batch start, independent of the search and display cap.
    frozen_order: HashMap<String, HashMap<String, usize>>,
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
    applied.list.sync_test_order(s, prev);
    apply_runtime(w, s, prev);
    apply_profiles(w, s, prev, &applied.list);
    apply_accounts(w, s, prev);
    apply_settings(w, s);
    if prev.is_none_or(|p| p.hosts != s.hosts) {
        apply_hosts(w, s);
    }
    if prev.is_none_or(|p| p.loopback.revision != s.loopback.revision) {
        super::loopback_editor::reset(w, &s.loopback.apps);
    }
    w.set_loopback_loading(s.loopback.loading);
    w.set_loopback_saving(s.loopback.saving);
    if prev.is_none_or(|p| p.rules != s.rules) {
        apply_rules(w, s, prev);
    }
    if prev.is_none_or(|p| p.logs.revision != s.logs.revision) {
        apply_logs(w, &s.logs, prev.map(|p| p.logs.as_ref()));
    }
    applied.last = Some(s.clone());
}

fn apply_runtime(w: &MainWindow, s: &AppStateView, prev: Option<&AppStateView>) {
    w.set_core_version(s.core_version.clone().into());
    let sidebar_status = if s.runtime.is_up() {
        let hours = s.connected_seconds / 3600;
        let minutes = s.connected_seconds % 3600 / 60;
        let seconds = s.connected_seconds % 60;
        format!("已连接 · {hours:02}:{minutes:02}:{seconds:02}")
    } else {
        status_text(&s.runtime)
    };
    w.set_status_text(status_text(&s.runtime).into());
    w.set_sidebar_status_text(sidebar_status.into());
    w.set_running(s.runtime.is_up());
    w.set_failed(matches!(s.runtime, RuntimeStatus::Failed(_)));
    let busy = !s.busy_tasks.is_empty()
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
        s.busy_tasks
            .iter()
            .next()
            .cloned()
            .or_else(|| busy.then(|| status_text(&s.runtime)))
            .unwrap_or_default()
            .into(),
    );
    w.set_mixed_port(i32::from(s.mixed_port));
    // M18: 只在值变化时写入，避免开关回弹
    if prev.is_none_or(|p| p.system_proxy_wanted != s.system_proxy_wanted) {
        w.set_system_proxy_wanted(s.system_proxy_wanted);
    }
    w.set_system_proxy_active(s.system_proxy_active);
    if prev.is_none_or(|p| p.tun_wanted != s.tun_wanted) {
        w.set_tun_wanted(s.tun_wanted);
    }
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

    let error = s.error.as_ref();
    w.set_error_stage(error.map(|e| e.stage.clone()).unwrap_or_default().into());
    w.set_error_text(error.map(|e| e.text.clone()).unwrap_or_default().into());
    w.set_error_retry(error.is_some_and(|e| e.retry.is_some()));
}

fn apply_profiles(
    w: &MainWindow,
    s: &AppStateView,
    prev: Option<&AppStateView>,
    list: &ListOptions,
) {
    let changed = |f: &dyn Fn(&AppStateView) -> bool| prev.is_none_or(f);
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
    if changed(&|p| p.groups_revision != s.groups_revision || p.routing_mode != s.routing_mode) {
        update_groups(w, &shown_groups(s), list);
    }
    w.set_testing_group(s.testing_group.clone().unwrap_or_default().into());
    w.set_testing_completed(i32::try_from(s.testing_completed).unwrap_or(i32::MAX));
    w.set_testing_total(i32::try_from(s.testing_total).unwrap_or(i32::MAX));
    if changed(&|p| p.policy_notes != s.policy_notes) {
        w.set_policy_notes(rc(s.policy_notes.iter().map(SharedString::from)));
    }
    if changed(&|p| p.check_lines != s.check_lines) {
        w.set_check_lines(rc(s.check_lines.iter().map(SharedString::from)));
    }
}

fn apply_accounts(w: &MainWindow, s: &AppStateView, prev: Option<&AppStateView>) {
    let changed = |f: &dyn Fn(&AppStateView) -> bool| prev.is_none_or(f);
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
}

fn apply_settings(w: &MainWindow, s: &AppStateView) {
    let st = &s.settings;
    w.set_settings_port(i32::from(st.mixed_port));
    w.set_settings_bypass(st.bypass.clone().into());
    w.set_settings_user_agent(st.default_user_agent.clone().into());
    w.set_settings_allow_lan(st.allow_lan);
    w.set_settings_auto_connect(st.auto_connect);
    w.set_settings_close_to_tray(st.close_to_tray);
    w.set_settings_autostart(st.autostart);
    w.set_settings_connection_log_limit(st.connection_log_limit.to_string().into());
    w.set_settings_operation_log_days(st.operation_log_days.to_string().into());
    w.set_settings_ipv6(st.ipv6.index() as i32);
    w.set_settings_unified_delay(st.unified_delay);
    w.set_settings_revision(st.revision as i32);
    w.set_geo_updating(s.geo_updating);
    w.set_exporting(s.exporting);
}

fn apply_rules(w: &MainWindow, s: &AppStateView, prev: Option<&AppStateView>) {
    let r = &s.rules;
    // A new saved state replaces the working copy; errors or notes alone do not.
    if prev.is_none_or(|p| p.rules.text != r.text || p.rules.enabled != r.enabled) {
        w.set_rules_saved_text(r.text.clone().into());
        w.set_rules_saved_enabled(r.enabled);
        super::rules_editor::reset(w, &r.text, r.enabled);
    }
    w.set_rules_count(i32::try_from(r.count).unwrap_or(i32::MAX));
    w.set_rules_errors(rc(r.errors.iter().map(|e| SharedString::from(e.as_str()))));
    w.set_rules_notes(rc(r.notes.iter().map(|e| SharedString::from(e.as_str()))));
    w.set_rules_saving(r.saving);
}

fn apply_hosts(w: &MainWindow, s: &AppStateView) {
    let h = &s.hosts;
    w.set_hosts_enabled(h.enabled);
    w.set_hosts_text(h.text.clone().into());
    w.set_hosts_count(i32::try_from(h.count).unwrap_or(i32::MAX));
    w.set_hosts_errors(rc(h.errors.iter().map(|e| SharedString::from(e.as_str()))));
    w.set_hosts_saving(h.saving);
}

/// Keeps nested node models alive when only latency or selection changes.
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
        let mut row = group_row(g, list);
        let Some(old) = current.row_data(i) else {
            continue;
        };
        let same_nodes = update_node_rows(&old.nodes, &row.nodes);
        if same_nodes {
            row.nodes = old.nodes.clone();
        }
        if !same_nodes
            || old.kind != row.kind
            || old.selectable != row.selectable
            || old.now != row.now
            || old.hidden != row.hidden
            || old.current_label != row.current_label
            || old.current_flag_index != row.current_flag_index
            || old.current_node_index != row.current_node_index
        {
            current.set_row_data(i, row);
        }
    }
}

fn update_node_rows(current: &ModelRc<NodeRow>, next: &ModelRc<NodeRow>) -> bool {
    if current.row_count() != next.row_count()
        || current
            .iter()
            .zip(next.iter())
            .any(|(a, b)| a.name != b.name)
    {
        return false;
    }
    for (i, row) in next.iter().enumerate() {
        if current.row_data(i).is_none_or(|old| old != row) {
            current.set_row_data(i, row);
        }
    }
    true
}

pub fn apply_tray(t: &AppTray, s: &AppStateView) {
    let state = match s.runtime {
        RuntimeStatus::Running | RuntimeStatus::Reloading => TrayState::Connected,
        RuntimeStatus::Restarting => TrayState::Restarting,
        RuntimeStatus::Failed(_) => TrayState::Error,
        RuntimeStatus::Starting => TrayState::Starting,
        RuntimeStatus::Stopping => TrayState::Stopping,
        RuntimeStatus::Stopped => TrayState::Idle,
    };
    t.set_state(state);
    let color = tray_color(state, s.routing_mode);
    t.set_state_icon(tray_icon(&color, s.tun_active));
    let selected = s.profiles.iter().find(|p| p.selected);
    let detail = match &s.runtime {
        RuntimeStatus::Failed(reason) => reason.clone(),
        runtime if runtime.is_up() => {
            let mode = s.routing_mode.map_or("未知模式", RoutingMode::label);
            let access = match (s.tun_active, s.system_proxy_active) {
                (true, true) => "TUN + 系统代理",
                (true, false) => "TUN",
                (false, true) => "系统代理",
                (false, false) => "仅本地内核",
            };
            let profile = selected.map_or_else(String::new, |p| {
                let node = exit_node(s);
                if node.is_empty() {
                    p.name.clone()
                } else {
                    format!("{} · {node}", p.name)
                }
            });
            if profile.is_empty() {
                format!("{mode}模式 · {access}")
            } else {
                format!("{mode}模式 · {access}\n{profile}")
            }
        }
        _ => s
            .routing_mode
            .map_or_else(String::new, |mode| format!("{}模式", mode.label())),
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

fn tray_color(state: TrayState, mode: Option<RoutingMode>) -> String {
    match state {
        TrayState::Error => "#DC2626".into(),
        TrayState::Starting | TrayState::Stopping | TrayState::Restarting => "#D97706".into(),
        TrayState::Connected => match mode {
            Some(RoutingMode::Rule) => "#2563EB".into(),
            Some(RoutingMode::Global) => "#9333EA".into(),
            Some(RoutingMode::Direct) => "#16A34A".into(),
            None => "#8A8F98".into(),
        },
        TrayState::Idle => "#8A8F98".into(),
    }
}

fn tray_icon(color: &str, tun_active: bool) -> Image {
    TRAY_ICONS.with_borrow_mut(|icons| {
        icons
            .entry((color.to_owned(), tun_active))
            .or_insert_with(|| {
                let mut svg = TRAY_ICON_SVG.replace("#16A34A", color);
                if tun_active {
                    svg = svg.replace(
                        "</svg>",
                        "<circle cx=\"230\" cy=\"790\" r=\"195\" fill=\"#111827\" stroke=\"#FFFFFF\" stroke-width=\"48\"/><path d=\"M115 665h230v68h-81v185h-68V733h-81z\" fill=\"#FFFFFF\"/></svg>",
                    );
                }
                render_tray_icon(&svg)
            })
            .clone()
    })
}

fn render_tray_icon(svg: &str) -> Image {
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())
        .expect("parse tray icon SVG");
    let mut pixmap = resvg::tiny_skia::Pixmap::new(64, 64).expect("allocate tray icon pixels");
    let scale = 64.0 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let pixels =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(pixmap.data(), 64, 64);
    Image::from_rgba8_premultiplied(pixels)
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
    if let Some(order) = list.frozen_order.get(&g.name) {
        nodes.sort_by_key(|n| order.get(&n.name).copied().unwrap_or(usize::MAX));
    } else if list.sort_by_delay {
        nodes.sort_by_key(|n| delay_sort_key(n));
    }
    let hidden = g.nodes.len() - nodes.len().min(MAX_LISTED_NODES);
    // Keep the current node reachable even when the unfiltered list hits its cap.
    if list.filter.is_empty()
        && let Some(index) = nodes.iter().position(|node| node.name == g.now)
        && index >= MAX_LISTED_NODES
    {
        nodes.swap(MAX_LISTED_NODES - 1, index);
    }
    nodes.truncate(MAX_LISTED_NODES);
    let current_node_index = nodes
        .iter()
        .position(|node| node.name == g.now)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1);
    let (current_label, current_flag_index) = flags::label(&g.now);
    GroupRow {
        name: g.name.clone().into(),
        kind: g.kind.clone().into(),
        now: g.now.clone().into(),
        current_label: current_label.into(),
        current_flag_index,
        current_node_index,
        selectable: g.selectable,
        hidden: i32::try_from(hidden).unwrap_or(i32::MAX),
        nodes: rc(nodes.into_iter().map(|n| node_row(n, &g.now))),
    }
}

fn node_row(node: &NodeView, current: &str) -> NodeRow {
    let (delay, delay_level, delay_detail) = node_delay_text(node);
    let (display_name, flag_index) = flags::label(&node.name);
    NodeRow {
        name: node.name.clone().into(),
        display_name: display_name.into(),
        flag_index,
        details: if node.transport.is_empty() {
            node.protocol.clone()
        } else {
            format!("{} · {}", node.protocol, node.transport)
        }
        .into(),
        delay,
        delay_level,
        delay_detail,
        current: node.name == current,
    }
}

fn node_delay_text(node: &NodeView) -> (SharedString, i32, SharedString) {
    let cached = node.delay.as_ref().filter(|d| d.is_fresh());
    let result = cached.map(|d| delay_result_text(d.outcome));
    let pending = match node.test_state {
        NodeTestState::Queued => "等待中",
        NodeTestState::Testing => "测试中…",
        NodeTestState::Idle => "",
    };
    if !pending.is_empty() {
        let previous = result.map(|(text, _)| format!("上次：{text}"));
        let detail = previous.map_or_else(|| pending.to_owned(), |p| format!("{pending} · {p}"));
        return (pending.into(), 0, detail.into());
    }
    let (text, level) = result.unwrap_or_default();
    let detail = cached.map_or_else(String::new, |d| d.detail.clone());
    (text.into(), level, detail.into())
}

fn delay_result_text(outcome: DelayOutcome) -> (String, i32) {
    match outcome {
        DelayOutcome::Reachable(ms) => (format!("{ms} ms"), if ms <= SLOW_MS { 1 } else { 2 }),
        DelayOutcome::Timeout => ("超时".into(), 3),
        DelayOutcome::Failed => ("失败".into(), 3),
    }
}

fn delay_sort_key(node: &NodeView) -> (u8, u32) {
    match node.delay.as_ref().map(|d| d.outcome) {
        Some(DelayOutcome::Reachable(ms)) => (0, ms),
        Some(_) => (1, 0),
        None => (2, 0),
    }
}

impl ListOptions {
    fn sync_test_order(&mut self, state: &AppStateView, prev: Option<&AppStateView>) {
        if state.testing_group.is_none() {
            self.frozen_order.clear();
        } else if self.frozen_order.is_empty() {
            for group in &prev.unwrap_or(state).groups {
                let mut nodes: Vec<_> = group.nodes.iter().collect();
                if self.sort_by_delay {
                    nodes.sort_by_key(|n| delay_sort_key(n));
                }
                let order = nodes
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| (n.name.clone(), i))
                    .collect();
                self.frozen_order.insert(group.name.clone(), order);
            }
        }
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

fn apply_logs(
    w: &MainWindow,
    page: &crate::app::logs::LogsPage,
    prev: Option<&crate::app::logs::LogsPage>,
) {
    w.set_log_status(page.status.clone().into());
    w.set_log_total(page.total as i32);
    if prev.is_none_or(|p| p.connections != page.connections) {
        w.set_connection_logs(rc(page.connections.iter().map(|r| super::ConnectionRow {
            id: r.id.to_string().into(),
            at: local_time(r.at, "%H:%M:%S").into(),
            protocol: r.protocol.clone().into(),
            address: r.address.clone().into(),
            action: r.action.clone().into(),
            rule: r.rule.clone().into(),
            profile: r.profile.clone().into(),
            node: r.node.clone().into(),
            details: format!(
                "时间：{}\n{}",
                local_time(r.at, "%Y-%m-%d %H:%M:%S"),
                r.details
            )
            .into(),
        })));
    }
    if prev.is_none_or(|p| p.operations != page.operations) {
        let mut previous_date = String::new();
        w.set_operation_logs(rc(page.operations.iter().map(|r| {
            let date = local_time(r.at, "%Y-%m-%d");
            let heading = if previous_date == date {
                String::new()
            } else {
                date.clone()
            };
            previous_date = date;
            super::OperationRow {
                at: local_time(r.at, "%H:%M:%S").into(),
                date: heading.into(),
                action: r.action.clone().into(),
                object: r.object.clone().into(),
                source: r.source.clone().into(),
                result: r.result.clone().into(),
                summary: r.summary.clone().into(),
            }
        })));
    }
}
fn local_time(at: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|t| t.with_timezone(&chrono::Local).format(format).to_string())
        .unwrap_or_default()
}
