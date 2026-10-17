//! Privileged lifecycle: content-only requests, shared policy, fixed binary.
use crate::{
    identity, log, paths,
    process::{self, CoreCommand, CoreHandle},
};
use mirage_ipc::{
    atomic, binary,
    policy::{self, ManagedOverlay},
    protocol::{ErrorKind, ServiceStatus},
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::Path,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, (ErrorKind, String)>;

#[derive(Default)]
struct State {
    last_exit: Option<i32>,
    running: Option<Running>,
}
struct Running {
    core: CoreHandle,
    started_at: u64,
    profile_id: String,
    controller: SocketAddr,
    secret: String,
}
#[derive(Default)]
pub struct CoreShell {
    state: Mutex<State>,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    pid: u32,
    created: u64,
    image: String,
}

pub struct Reload {
    pub controller: SocketAddr,
    pub secret: String,
    pub config: std::path::PathBuf,
}

impl CoreShell {
    pub fn clean_leftover(&self) {
        let Ok(home) = paths::tun_home() else { return };
        let recorded = std::fs::read(home.join("core.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<Journal>(&data).ok());
        let _ = self.stop();
        if let Some(j) = recorded
            && let Ok(exe) = paths::core_exe()
            && identity::same_path(&j.image, &exe.to_string_lossy())
        {
            // Identity is checked on the same process handle used to terminate.
            let _ = identity::terminate_matching(j.pid, j.created, &j.image);
        }
        clear_files(&home);
    }

    pub fn status(&self) -> ServiceStatus {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(code) = state.running.as_mut().and_then(|r| r.core.try_exit()) {
            state.last_exit = Some(code);
            state.running = None;
            if let Ok(home) = paths::tun_home() {
                clear_files(&home);
            }
        }
        ServiceStatus {
            running: state.running.is_some(),
            pid: state.running.as_ref().map_or(0, |r| r.core.pid),
            started_at: state.running.as_ref().map(|r| r.started_at),
            last_exit: state.last_exit,
        }
    }

    pub fn stop(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| fail(ErrorKind::Busy, "服务状态不可用"))?;
        if let Some(mut running) = state.running.take() {
            running.core.stop(Duration::from_secs(3));
            state.last_exit = running.core.try_exit();
            log::info(&format!("core stopped pid={}", running.core.pid));
        }
        if let Ok(home) = paths::tun_home() {
            clear_files(&home);
        }
        Ok(())
    }

    pub fn start(&self, yaml: &str, port: u16, secret: &str, profile: &str) -> Result<u32> {
        self.status();
        let mut state = self
            .state
            .lock()
            .map_err(|_| fail(ErrorKind::Busy, "服务状态不可用"))?;
        if state.running.is_some() {
            return Err(fail(ErrorKind::AlreadyRunning, "已有内核运行"));
        }
        if port == 0 || secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(fail(
                ErrorKind::PolicyRejected,
                "控制器端口或 secret 不合法",
            ));
        }
        let controller = SocketAddr::from(([127, 0, 0, 1], port));
        let config = prepare(yaml, controller, secret, profile)?;
        let home = paths::tun_home().map_err(io_error)?;
        let core = process::spawn(CoreCommand {
            exe: paths::core_exe().map_err(io_error)?,
            home_dir: home.clone(),
            config_file: config,
            controller,
            secret: secret.to_owned(),
            log_file: home.join("logs").join("core.log"),
        })
        .map_err(io_error)?;
        let pid = core.pid;
        let id = identity::identity(pid).ok_or_else(|| fail(ErrorKind::Io, "内核身份不可读"))?;
        let journal = Journal {
            pid,
            created: id.created,
            image: id.image,
        };
        let data = serde_json::to_vec(&journal).map_err(io_error)?;
        atomic::write(&home.join("core.json"), &data).map_err(io_error)?;
        state.last_exit = None;
        state.running = Some(Running {
            core,
            started_at: now(),
            profile_id: profile.to_owned(),
            controller,
            secret: secret.to_owned(),
        });
        log::info(&format!("core started pid={pid}"));
        Ok(pid)
    }

    pub fn prepare_reload(&self, yaml: &str) -> Result<Reload> {
        let state = self
            .state
            .lock()
            .map_err(|_| fail(ErrorKind::Busy, "服务状态不可用"))?;
        let r = state
            .running
            .as_ref()
            .ok_or_else(|| fail(ErrorKind::CoreNotRunning, "内核未运行"))?;
        let config = prepare(yaml, r.controller, &r.secret, &r.profile_id)?;
        Ok(Reload {
            config,
            controller: r.controller,
            secret: r.secret.clone(),
        })
    }
}

fn prepare(
    yaml: &str,
    controller: SocketAddr,
    secret: &str,
    profile: &str,
) -> Result<std::path::PathBuf> {
    let root = policy::parse(yaml).map_err(|e| fail(ErrorKind::PolicyRejected, e))?;
    let mixed = root
        .get("mixed-port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p >= 1024 && *p != controller.port())
        .ok_or_else(|| fail(ErrorKind::PolicyRejected, "mixed-port 不合法"))?;
    let (yaml, _) = policy::apply(
        yaml,
        &ManagedOverlay {
            mixed_port: mixed,
            controller,
            secret,
            log_level: "warning",
            mode: None,
            tun: Some(Default::default()),
            profile_id: profile,
        },
    )
    .map_err(|e| fail(ErrorKind::PolicyRejected, e))?;
    let exe = paths::core_exe().map_err(io_error)?;
    if binary::hash_file(&exe).map_err(io_error)? != binary::EXPECTED_SHA256 {
        return Err(fail(
            ErrorKind::CheckFailed,
            "官方内核 SHA256 不匹配，请更新服务",
        ));
    }
    let home = paths::tun_home().map_err(io_error)?;
    let check = home.join("check.yaml");
    atomic::write(&check, yaml.as_bytes()).map_err(io_error)?;
    let result = process::check_config(&exe, &home, &check);
    let _ = std::fs::remove_file(&check);
    result.map_err(|e| fail(ErrorKind::CheckFailed, e))?;
    let config = home.join("config.yaml");
    atomic::write(&config, yaml.as_bytes()).map_err(io_error)?;
    Ok(config)
}

pub async fn reload(prepared: Reload) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(io_error)?;
    let response = client
        .put(format!("http://{}/configs?force=true", prepared.controller))
        .bearer_auth(&prepared.secret)
        .json(&serde_json::json!({"path": prepared.config}))
        .send()
        .await
        .map_err(|_| fail(ErrorKind::Io, "服务内核重载请求失败"))?;
    if !response.status().is_success() {
        return Err(fail(
            ErrorKind::CheckFailed,
            format!("服务内核拒绝重载：{}", response.status()),
        ));
    }
    Ok(())
}

fn clear_files(home: &Path) {
    for file in ["core.json", "config.yaml", "check.yaml"] {
        let _ = std::fs::remove_file(home.join(file));
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn fail(kind: ErrorKind, detail: impl ToString) -> (ErrorKind, String) {
    (kind, detail.to_string())
}
fn io_error(e: impl std::fmt::Display) -> (ErrorKind, String) {
    fail(ErrorKind::Io, e)
}
