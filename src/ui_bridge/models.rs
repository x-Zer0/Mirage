//! Converts [`AppStateView`] snapshots into Slint properties.

use super::{AccountRow, AppTray, GroupRow, MainWindow, NodeRow, ProfileRow, TrayState};
use crate::app::state::{AppStateView, Connectivity, RuntimeStatus};
use crate::app::status_text;
use crate::cores::mihomo::controller::ProxyGroupView;
use slint::{ModelRc, SharedString, VecModel};
use std::rc::Rc;

const SLOW_MS: u32 = 500;

pub fn apply(w: &MainWindow, s: &AppStateView) {
    w.set_core_version(s.core_version.clone().into());
    w.set_status_text(status_text(&s.runtime).into());
    w.set_running(s.runtime.is_up());
    let busy =
        s.busy.is_some() || matches!(s.runtime, RuntimeStatus::Starting | RuntimeStatus::Stopping);
    w.set_busy(busy);
    w.set_busy_text(s.busy.clone().unwrap_or_default().into());
    w.set_mixed_port(i32::from(s.mixed_port));
    w.set_system_proxy_wanted(s.system_proxy_wanted);
    w.set_system_proxy_active(s.system_proxy_active);
    let (text, level) = connectivity_text(s);
    w.set_connectivity_text(text.into());
    w.set_connectivity_level(level);
    let selected = s.profiles.iter().find(|p| p.selected);
    w.set_profile_name(selected.map(|p| p.name.clone()).unwrap_or_default().into());
    w.set_profiles(rc(s.profiles.iter().map(|p| ProfileRow {
        id: p.id.clone().into(),
        name: p.name.clone().into(),
        source: p.source.clone().into(),
        detail: p.detail.clone().into(),
        updated: p.updated.clone().into(),
        selected: p.selected,
        remote: p.remote,
        updating: p.updating,
        usage: p.usage.clone().into(),
        fetch_error: p.fetch_error.clone().into(),
    })));
    w.set_groups(rc(s.groups.iter().map(group_row)));
    w.set_testing_group(s.testing_group.clone().unwrap_or_default().into());
    w.set_policy_notes(rc(s.policy_notes.iter().map(SharedString::from)));
    w.set_check_lines(rc(s.check_lines.iter().map(SharedString::from)));
    w.set_notice(
        s.notice
            .as_ref()
            .map(|n| n.text.clone())
            .unwrap_or_default()
            .into(),
    );
    w.set_notice_error(s.notice.as_ref().is_some_and(|n| n.error));
    w.set_accounts(rc(s.accounts.iter().map(|a| AccountRow {
        id: a.id.clone().into(),
        name: a.name.clone().into(),
        status: a.status.clone().into(),
        status_level: a.status_level,
        busy: a.busy,
        plan: a.plan.clone().into(),
        usage: a.usage.clone().into(),
        expires: a.expires.clone().into(),
        has_profile: a.has_profile,
    })));
    w.set_login_busy(s.login_busy);
    w.set_login_message(s.login_message.clone().unwrap_or_default().into());
    w.set_probing_hosts(s.probing_hosts);
    w.set_provider_hosts(rc(s.provider_hosts.iter().map(SharedString::from)));
}

pub fn apply_tray(t: &AppTray, s: &AppStateView) {
    t.set_state(match s.runtime {
        RuntimeStatus::Running | RuntimeStatus::Reloading => TrayState::Connected,
        RuntimeStatus::Restarting => TrayState::Restarting,
        RuntimeStatus::Failed(_) => TrayState::Error,
        _ => TrayState::Idle,
    });
    t.set_system_proxy(s.system_proxy_wanted);
    t.set_running(s.runtime.is_up());
    t.set_busy(matches!(
        s.runtime,
        RuntimeStatus::Starting | RuntimeStatus::Stopping
    ));
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

fn group_row(g: &ProxyGroupView) -> GroupRow {
    GroupRow {
        name: g.name.clone().into(),
        kind: g.kind.clone().into(),
        now: g.now.clone().into(),
        selectable: g.selectable,
        nodes: rc(g.nodes.iter().map(|n| {
            let (delay, level) = match n.delay {
                None => (SharedString::new(), 0),
                Some(0) => ("超时".into(), 3),
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

fn rc<T: Clone + 'static>(items: impl Iterator<Item = T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(items.collect::<Vec<_>>())))
}
