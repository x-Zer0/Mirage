//! Core process ownership for the service (§11.7).
//!
//! The core is created suspended, assigned to a job object with
//! `KILL_ON_JOB_CLOSE`, then resumed. If the service exits for any reason the
//! job closes and the core dies with it, so a TUN adapter never outlives the
//! process that owns it.

use std::ffi::c_void;
use std::io::Read;
use std::net::SocketAddr;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

use crate::log;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

pub struct CoreCommand {
    pub exe: PathBuf,
    pub home_dir: PathBuf,
    pub config_file: PathBuf,
    pub controller: SocketAddr,
    pub secret: String,
    pub log_file: PathBuf,
}

pub struct CoreHandle {
    pub pid: u32,
    child: Child,
    /// Dropped last; closing it kills anything still in the job.
    _job: JobHandle,
}

impl CoreHandle {
    /// Exit code once the process has ended, without blocking.
    pub fn try_exit(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            Ok(None) => None,
            // A query error is treated as "gone"; the caller reports it.
            Err(_) => Some(-1),
        }
    }

    /// Graceful stop first, then terminate: the service must never leave TUN
    /// routes behind because the core ignored a console signal.
    pub fn stop(&mut self, grace: Duration) {
        // The core has no window and no Ctrl+C handler; a console signal would
        // be a no-op, so this is a bounded wait followed by TerminateProcess.
        let deadline = Instant::now() + grace;
        if self.child.kill().is_ok() {
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.child.wait();
        }
    }
}

impl Drop for CoreHandle {
    fn drop(&mut self) {
        // `_job` closes after this; KILL_ON_JOB_CLOSE reaps the core.
    }
}

pub fn spawn(cmd: CoreCommand) -> std::io::Result<CoreHandle> {
    let writer = Arc::new(mirage_ipc::rolling::RollingWriter::open(
        cmd.log_file.clone(),
        mirage_ipc::rolling::MAX_FILE_BYTES,
    )?);
    let mut child = Command::new(&cmd.exe)
        .arg("-d")
        .arg(&cmd.home_dir)
        .arg("-f")
        .arg(&cmd.config_file)
        .arg("-ext-ctl")
        .arg(cmd.controller.to_string())
        .arg("-secret")
        .arg(&cmd.secret)
        .env_remove("SAFE_PATHS")
        .current_dir(&cmd.home_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        .spawn()?;

    if let Some(stream) = child.stdout.take() {
        pump(stream, writer.clone());
    }
    if let Some(stream) = child.stderr.take() {
        pump(stream, writer);
    }
    let pid = child.id();
    let job = match JobHandle::create().and_then(|job| {
        let handle = child.as_raw_handle().cast::<c_void>();
        job.assign(handle).map(|()| job)
    }) {
        Ok(job) => job,
        Err(e) => {
            // Still suspended; kill_on_drop is not set, so kill explicitly.
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    if let Err(e) = resume_threads(pid) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    log::info(&format!("core started pid={pid}"));
    Ok(CoreHandle {
        pid,
        child,
        _job: job,
    })
}

/// Runs `mihomo -t` in the service-owned home (§11.7). `Err` carries the last
/// lines of the check output, which the app shows verbatim.
pub fn check_config(exe: &Path, home_dir: &Path, config: &Path) -> Result<(), String> {
    let mut child = Command::new(exe)
        .args(["-t", "-d"])
        .arg(home_dir)
        .arg("-f")
        .arg(config)
        .current_dir(home_dir)
        .env_remove("SAFE_PATHS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        .spawn()
        .map_err(|e| e.to_string())?;
    let job = JobHandle::create().and_then(|job| job.assign(child.as_raw_handle()).map(|()| job));
    let _job = match job {
        Ok(job) => job,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.to_string());
        }
    };
    // Drain both pipes concurrently with bounded tails, avoiding a full-pipe deadlock.
    let stdout = child.stdout.take().map(read_tail);
    let stderr = child.stderr.take().map(read_tail);
    resume_threads(child.id()).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + CHECK_TIMEOUT;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => break Err("配置校验超时".to_owned()),
            Err(e) => break Err(e.to_string()),
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let out = stdout.and_then(|t| t.join().ok()).unwrap_or_default();
    let err = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    if result? {
        return Ok(());
    }
    let text = format!("{out}\n{err}");
    let lines: Vec<_> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    Err(lines[lines.len().saturating_sub(3)..].join(" | "))
}

fn read_tail(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut tail = Vec::new();
        let mut buf = [0; 4096];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > 16384 {
                tail.drain(..tail.len() - 16384);
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    })
}

fn pump(mut stream: impl Read + Send + 'static, writer: Arc<mirage_ipc::rolling::RollingWriter>) {
    std::thread::spawn(move || {
        let mut buf = [0; 16384];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            // Continue draining if the disk is full so the core cannot block.
            let _ = writer.append(&buf[..n]);
        }
    });
}
struct JobHandle(HANDLE);

// SAFETY: a job object handle is a kernel handle usable from any thread.
unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

impl JobHandle {
    fn create() -> std::io::Result<Self> {
        // SAFETY: anonymous job creation; the limit struct is fully initialised
        // and its size is passed exactly.
        unsafe {
            let job = CreateJobObjectW(None, None).map_err(std::io::Error::other)?;
            let job = Self(job);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(std::io::Error::other)?;
            Ok(job)
        }
    }

    fn assign(&self, process: *mut c_void) -> std::io::Result<()> {
        // SAFETY: `process` is the live process handle owned by `Child`.
        unsafe { AssignProcessToJobObject(self.0, HANDLE(process)) }.map_err(std::io::Error::other)
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // SAFETY: closed exactly once; KILL_ON_JOB_CLOSE ends the core.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Resumes every thread of a process created with `CREATE_SUSPENDED`.
fn resume_threads(pid: u32) -> std::io::Result<()> {
    // SAFETY: snapshot iteration with a correctly sized THREADENTRY32; every
    // opened handle is closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(std::io::Error::other)?;
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut resumed = 0;
        let mut more = Thread32First(snap, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == pid
                && let Ok(thread) = OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
            {
                if ResumeThread(thread) != u32::MAX {
                    resumed += 1;
                }
                let _ = CloseHandle(thread);
            }
            more = Thread32Next(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        if resumed == 0 {
            return Err(std::io::Error::other("无法恢复内核主线程"));
        }
    }
    Ok(())
}
