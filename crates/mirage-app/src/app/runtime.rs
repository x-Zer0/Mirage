//! Starting and stopping the core for a profile (DESIGN.md §5.2, §16.2).

use super::pipeline;
use crate::cores::mihomo::binary::CoreBinary;
use crate::cores::mihomo::compose::{self, ManagedOverlay};
use crate::cores::mihomo::controller::{Controller, GLOBAL_GROUP};
use crate::cores::mihomo::launcher::Process;
use crate::cores::mihomo::process::{self, CoreCommand};
use crate::cores::mihomo::service::{ServiceClient, ServiceProcess};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{Profile, ProfileId, RevisionId, RoutingMode};
use crate::platform::windows::{process as win_process, tun as win_tun};
use crate::storage::journal::{self, CoreJournal};
use crate::storage::layout::Layout;
use crate::storage::{atomic, meta};
use crate::subscriptions::{detect, revisions};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const STAGE: &str = "app.runtime";
pub const CORE_LOG_LEVEL: &str = "warning";
/// Only used for `-t` during import; the check does not bind it.
pub const PLACEHOLDER_CONTROLLER: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9));
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(200);
const STOP_GRACE: Duration = Duration::from_secs(4);

pub struct Running {
    pub generation: u64,
    pub profile_id: ProfileId,
    pub process: Process,
    pub controller: Controller,
    pub controller_addr: SocketAddr,
    /// Controller secret; memory only (the run config holds it under a private ACL).
    pub secret: String,
    pub mixed_port: u16,
    pub log_file: PathBuf,
    pub check_lines: Vec<String>,
    pub policy_notes: Vec<String>,
    pub started_at: Instant,
}

pub struct StartRequest {
    pub generation: u64,
    pub layout: Layout,
    pub binary: CoreBinary,
    pub profile: Profile,
    pub mixed_port: u16,
    pub mode: Option<RoutingMode>,
    pub tun: bool,
}

pub async fn start(req: StartRequest) -> AppResult<Running> {
    let rev_id = req
        .profile
        .active_revision
        .clone()
        .ok_or_else(|| AppError::new(AppErrorKind::NotAConfig, STAGE, "该配置没有可用版本"))?;
    if !req.tun
        && req.layout.recovery.join("tun.json").exists()
        && crate::platform::windows::service::installed()
    {
        crate::cores::mihomo::service::cleanup().await?;
        journal::remove(&req.layout.recovery.join("tun.json"));
    }
    ensure_port_free(req.mixed_port)?;
    let controller_addr = free_loopback_addr()?;
    let secret = random_secret()?;

    let raw = revisions::read_raw(&req.layout, &req.profile, &rev_id)?;
    let overlay = ManagedOverlay {
        mixed_port: req.mixed_port,
        controller: controller_addr,
        secret: &secret,
        log_level: CORE_LOG_LEVEL,
        profile_id: &req.profile.id,
        mode: req.mode,
        tun: req.tun,
    };
    let prepared = pipeline::prepare(&req.binary, &req.layout, &rev_id, &raw, &overlay).await?;
    let run_config = req.layout.run_config();
    atomic::write(&run_config, prepared.composed.yaml.as_bytes())
        .map_err(|e| AppError::storage(STAGE, e))?;

    // §18: the core log uses the same bounded rolling scheme as the app log;
    // this is the stem, and the writer stamps the dated file names itself.
    let log_file = if req.tun {
        mirage_ipc::known_folder::service_data()
            .map_err(|e| AppError::storage(STAGE, e))?
            .join("tun-home")
            .join("logs")
            .join("core.log")
    } else {
        req.layout.core_log_stem()
    };
    let controller = Controller::new(controller_addr, &secret)?;
    let process = launch(
        &req,
        &prepared.composed.yaml,
        controller_addr,
        &secret,
        &log_file,
    )
    .await?;
    tracing::info!(pid = process.pid(), %controller_addr, "core spawned");
    if let Err(e) = wait_ready(&process, &controller, req.mixed_port, &log_file).await {
        if process.terminate(STOP_GRACE).await {
            journal::remove(&req.layout.recovery.join("tun.json"));
        }
        journal::remove(&journal::core_path(&req.layout));
        remove_run_config(&req.layout);
        return Err(e);
    }
    restore_selection(&req.layout, &req.profile.id, &controller).await;
    Ok(Running {
        generation: req.generation,
        profile_id: req.profile.id,
        process,
        controller,
        controller_addr,
        secret,
        mixed_port: req.mixed_port,
        log_file,
        check_lines: prepared.report.lines,
        policy_notes: prepared.composed.policy_notes,
        started_at: Instant::now(),
    })
}

async fn launch(
    req: &StartRequest,
    yaml: &str,
    controller: SocketAddr,
    secret: &str,
    log_file: &std::path::Path,
) -> AppResult<Process> {
    if req.tun {
        let path = req.layout.recovery.join("tun.json");
        match ServiceProcess::start(
            yaml.to_owned(),
            req.profile.id.clone(),
            controller.port(),
            secret.to_owned(),
            &path,
        )
        .await
        {
            Ok(process) => return Ok(Process::Service(process)),
            Err(e) => {
                // Wait for the service to finish disposing any partial start.
                if path.exists() && crate::cores::mihomo::service::cleanup().await.is_ok() {
                    journal::remove(&path);
                }
                return Err(e);
            }
        }
    }
    let process = process::spawn(CoreCommand {
        exe: req.binary.exe.clone(),
        home_dir: req.layout.core_home.clone(),
        config_file: req.layout.run_config(),
        controller,
        secret: secret.to_owned(),
        log_file: log_file.to_path_buf(),
    })?;
    record_core(&req.layout, process.pid);
    Ok(Process::Direct(process))
}
pub struct ReloadRequest {
    pub layout: Layout,
    pub profile: Profile,
    pub controller: Controller,
    pub controller_addr: SocketAddr,
    pub secret: String,
    pub mixed_port: u16,
    pub mode: Option<RoutingMode>,
    pub tun: bool,
    pub candidate: RevisionId,
    pub service: Option<ServiceClient>,
    /// last_good, written back when the candidate fails (§8 step 9).
    pub fallback: Option<RevisionId>,
}

#[derive(Debug)]
pub enum ReloadOutcome {
    Applied,
    /// Candidate rejected; the core runs last_good again.
    RolledBack(String),
    /// Neither candidate nor last_good could be hot-applied; restart the core.
    NeedsRestart(String),
}

/// Hot-applies a checked revision to the running core, rolling back to
/// last_good on failure (§8 steps 7–9, §16.4).
pub async fn reload(req: ReloadRequest) -> ReloadOutcome {
    let reason = match apply_revision(&req, &req.candidate).await {
        Ok(()) => return ReloadOutcome::Applied,
        Err(e) => {
            tracing::warn!(error = %e, "hot reload failed, rolling back");
            e.user_message()
        }
    };
    let Some(fallback) = req.fallback.as_ref().filter(|f| **f != req.candidate) else {
        return ReloadOutcome::NeedsRestart(reason);
    };
    match apply_revision(&req, fallback).await {
        Ok(()) => ReloadOutcome::RolledBack(reason),
        Err(e) => {
            tracing::error!(error = %e, "rollback reload failed");
            ReloadOutcome::NeedsRestart(reason)
        }
    }
}

/// compose → run/config.yaml → `PUT /configs` → `/proxies` readable → selection.
async fn apply_revision(req: &ReloadRequest, rev: &RevisionId) -> AppResult<()> {
    let raw = revisions::read_raw(&req.layout, &req.profile, rev)?;
    let detected = detect::detect(&raw)?;
    let overlay = ManagedOverlay {
        mixed_port: req.mixed_port,
        controller: req.controller_addr,
        secret: &req.secret,
        log_level: CORE_LOG_LEVEL,
        profile_id: &req.profile.id,
        mode: req.mode,
        tun: req.tun,
    };
    let composed = compose::compose(&detected.text, &overlay)?;
    let run_config = req.layout.run_config();
    atomic::write(&run_config, composed.yaml.as_bytes())
        .map_err(|e| AppError::storage(STAGE, e))?;
    if let Some(service) = &req.service {
        service.reload(composed.yaml).await?;
    } else {
        req.controller.reload(&run_config).await?;
    }
    let groups = req.controller.groups().await?;
    let declared = groups.iter().filter(|g| g.name != GLOBAL_GROUP).count();
    if declared == 0 && composed.summary.groups > 0 {
        return Err(AppError::new(
            AppErrorKind::CoreNotReady,
            STAGE,
            "重载后读不到代理组",
        ));
    }
    restore_selection(&req.layout, &req.profile.id, &req.controller).await;
    Ok(())
}

/// Closes connections, terminates the owned process and removes the run config.
pub async fn stop(running: Running, layout: &Layout) {
    let close = running.controller.close_all_connections();
    if tokio::time::timeout(Duration::from_secs(2), close)
        .await
        .is_err()
    {
        tracing::debug!("close connections timed out");
    }
    if running.process.terminate(STOP_GRACE).await {
        journal::remove(&layout.recovery.join("tun.json"));
    }
    forget_exited(layout);
    tracing::info!("core stopped");
}

/// Cleanup after the core process is gone (stopped or crashed).
pub fn forget_exited(layout: &Layout) {
    journal::remove(&journal::core_path(layout));
    remove_run_config(layout);
}

/// Journals PID + creation time + image so a crash of Mirage itself can be
/// cleaned up on next start (§10.4). The Job Object normally makes this moot.
fn record_core(layout: &Layout, pid: u32) {
    let Some(identity) = win_process::identity(pid) else {
        tracing::warn!(pid, "core identity not readable; not journaled");
        return;
    };
    let mut entry = CoreJournal {
        pid,
        created: identity.created,
        image: identity.image,
        ..Default::default()
    };
    if let Err(e) = journal::save_core(layout, &mut entry) {
        tracing::warn!(error = %e, "core journal not written");
    }
}

pub fn remove_run_config(layout: &Layout) {
    match std::fs::remove_file(layout.run_config()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "run config not removed"),
    }
}

async fn wait_ready(
    process: &Process,
    controller: &Controller,
    mixed_port: u16,
    log_file: &std::path::Path,
) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        if process.has_exited() {
            return Err(AppError::new(
                AppErrorKind::CoreExited,
                STAGE,
                process::log_tail(log_file, 3),
            ));
        }
        if controller.version().await.is_ok()
            && port_accepts(mixed_port).await
            && (matches!(process, Process::Direct(_)) || tun_ready(controller).await)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::new(
                AppErrorKind::CoreNotReady,
                STAGE,
                if matches!(process, Process::Service(_)) {
                    "TUN 内核、网卡或路由 15 秒内未就绪"
                } else {
                    "内核 15 秒内未就绪"
                },
            ));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

async fn tun_ready(controller: &Controller) -> bool {
    let Ok(Some(device)) = controller.tun_device().await else {
        return false;
    };
    tokio::task::spawn_blocking(move || win_tun::is_ready(&device))
        .await
        .unwrap_or(false)
}

async fn port_accepts(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect(addr))
        .await
        .is_ok_and(|r| r.is_ok())
}

/// Re-applies remembered choices to Selector groups; vanished nodes are skipped (§10.6).
async fn restore_selection(layout: &Layout, profile_id: &ProfileId, controller: &Controller) {
    let saved: meta::SelectionFile = match meta::load(&layout.selection_file(profile_id), STAGE) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "selection not loaded");
            return;
        }
    };
    if saved.groups.is_empty() {
        return;
    }
    let Ok(groups) = controller.groups().await else {
        return;
    };
    for group in groups.iter().filter(|g| g.selectable) {
        let Some(node) = saved.groups.get(&group.name) else {
            continue;
        };
        if group.now != *node
            && group.nodes.iter().any(|n| &n.name == node)
            && let Err(e) = controller.select(&group.name, node).await
        {
            tracing::warn!(error = %e, "restore selection failed");
        }
    }
}

fn ensure_port_free(port: u16) -> AppResult<()> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map(drop)
        .map_err(|_| {
            // §11.8: name the process holding the port, so the user can act.
            let owner = crate::platform::windows::conflict::port_owner(port);
            let hint = match owner {
                Some(name) => format!("，占用进程：{name}"),
                None => String::new(),
            };
            AppError::new(
                AppErrorKind::PortInUse,
                STAGE,
                format!("127.0.0.1:{port} 已被其他程序占用{hint}；可在设置中修改本地代理端口"),
            )
        })
}

fn free_loopback_addr() -> AppResult<SocketAddr> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| AppError::new(AppErrorKind::PortInUse, STAGE, e.to_string()))?;
    listener
        .local_addr()
        .map_err(|e| AppError::internal(STAGE, e))
}

/// 32 random bytes, hex encoded; lives only in memory and the private run config.
pub fn random_secret() -> AppResult<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| AppError::internal(STAGE, e))?;
    Ok(hex::encode(bytes))
}
