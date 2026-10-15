//! System proxy transaction (DESIGN.md §11.3). The journal is written before
//! WinINet is touched, so a crash at any point can be undone on next start,
//! and a restore never overwrites settings someone else changed afterwards.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::platform::windows::sysproxy::{self, FLAG_DIRECT, FLAG_PROXY, ProxySettings};
use crate::storage::journal::{self, SysProxyJournal, SysProxyState};
use crate::storage::layout::Layout;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

const STAGE: &str = "app.sysproxy";
pub const DEFAULT_BYPASS: &str = "<local>;localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;\
172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;\
172.29.*;172.30.*;172.31.*;192.168.*";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restore {
    /// Our setting was still in place and the original was written back.
    Restored,
    /// Someone changed the setting after us; it was left alone.
    KeptExternal,
    /// No journal, or our setting was never written.
    Nothing,
}

impl Restore {
    pub fn notice(self) -> Option<&'static str> {
        match self {
            Self::KeptExternal => Some("系统代理已被其他程序修改，未还原"),
            _ => None,
        }
    }
}

pub fn applied_settings(mixed_port: u16) -> ProxySettings {
    ProxySettings {
        flags: FLAG_DIRECT | FLAG_PROXY,
        server: Some(server_for(mixed_port)),
        bypass: Some(DEFAULT_BYPASS.to_owned()),
        autoconfig_url: None,
    }
}

fn server_for(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// Points the system proxy at the local core (steps 1–6 of §11.3).
pub fn enable(layout: &Layout, mixed_port: u16) -> AppResult<()> {
    let mut original = sysproxy::read().map_err(|e| sysproxy_error(e.to_string()))?;
    if points_at(&original, mixed_port) {
        // Leftover from an earlier crash: restoring it would leave a dead proxy.
        original.flags &= !FLAG_PROXY;
        original.flags |= FLAG_DIRECT;
    }
    let mut journal = SysProxyJournal {
        state: SysProxyState::Pending,
        original,
        applied: applied_settings(mixed_port),
        ..Default::default()
    };
    journal::save_sysproxy(layout, &mut journal)?;
    if !port_accepts(mixed_port) {
        journal::remove(&journal::sysproxy_path(layout));
        return Err(AppError::new(
            AppErrorKind::CoreNotReady,
            STAGE,
            format!("本地端口 {mixed_port} 无法连接，未开启系统代理"),
        ));
    }
    if let Err(e) = sysproxy::write(&journal.applied) {
        journal::remove(&journal::sysproxy_path(layout));
        return Err(sysproxy_error(e.to_string()));
    }
    journal.state = SysProxyState::Applied;
    journal::save_sysproxy(layout, &mut journal)?;
    tracing::info!(port = mixed_port, "system proxy applied");
    Ok(())
}

/// Undoes [`enable`] if our setting is still the current one (close flow of §11.3).
/// Also used by startup recovery for a journal left by a crash.
pub fn restore(layout: &Layout) -> AppResult<Restore> {
    let Some(journal) = journal::load_sysproxy(layout)? else {
        return Ok(Restore::Nothing);
    };
    let current = sysproxy::read().map_err(|e| sysproxy_error(e.to_string()))?;
    let outcome = if current.same_route(&journal.applied) {
        sysproxy::write(&journal.original).map_err(|e| sysproxy_error(e.to_string()))?;
        Restore::Restored
    } else if journal.state == SysProxyState::Pending && current.same_route(&journal.original) {
        Restore::Nothing
    } else {
        Restore::KeptExternal
    };
    journal::remove(&journal::sysproxy_path(layout));
    tracing::info!(?outcome, "system proxy restore");
    Ok(outcome)
}

/// `true` while the journal says we applied it and WinINet still shows our value.
pub fn still_ours(layout: &Layout) -> AppResult<bool> {
    let Some(journal) = journal::load_sysproxy(layout)? else {
        return Ok(false);
    };
    let current = sysproxy::read().map_err(|e| sysproxy_error(e.to_string()))?;
    Ok(current.same_route(&journal.applied))
}

/// Forgets the journal without touching WinINet (after an external change).
pub fn forget(layout: &Layout) {
    journal::remove(&journal::sysproxy_path(layout));
}

fn points_at(settings: &ProxySettings, port: u16) -> bool {
    settings.proxy_enabled()
        && settings.server.as_deref().is_some_and(|s| {
            let ours = server_for(port);
            s == ours || s.split(';').any(|part| part.ends_with(&format!("={ours}")))
        })
}

fn port_accepts(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

fn sysproxy_error(detail: String) -> AppError {
    AppError::new(AppErrorKind::SysProxyConflict, STAGE, detail)
}
