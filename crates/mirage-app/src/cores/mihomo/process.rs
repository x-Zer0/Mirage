//! Core process ownership (DESIGN.md §10.4): the core is created suspended,
//! assigned to a kill-on-close Job Object, then resumed, so it never runs
//! unowned. Closing the job (or Mirage exiting) terminates the core.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::logging::{MAX_FILE_BYTES, RollingWriter};
use std::collections::HashMap;
use std::ffi::c_void;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{oneshot, watch};
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

const STAGE: &str = "mihomo.process";
pub const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW
const SUSPENDED: u32 = 0x0000_0004; // CREATE_SUSPENDED

pub struct CoreCommand {
    pub exe: PathBuf,
    pub home_dir: PathBuf,
    pub config_file: PathBuf,
    pub controller: SocketAddr,
    pub secret: String,
    pub log_file: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub struct ExitInfo {
    pub code: Option<i32>,
}

pub struct CoreProcess {
    pub pid: u32,
    _job: JobHandle,
    kill: Option<oneshot::Sender<()>>,
    exit: watch::Receiver<Option<ExitInfo>>,
}

impl CoreProcess {
    pub fn exit_watch(&self) -> watch::Receiver<Option<ExitInfo>> {
        self.exit.clone()
    }

    pub fn has_exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    /// Terminates the owned process and waits up to `grace` for it to be reaped.
    pub async fn terminate(mut self, grace: Duration) {
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
        let mut exit = self.exit.clone();
        let reaped = tokio::time::timeout(grace, exit.wait_for(Option::is_some)).await;
        if reaped.is_err() {
            // Dropping the job handle below still kills it via KILL_ON_JOB_CLOSE.
            tracing::warn!(pid = self.pid, "core did not exit in time");
        }
    }
}

pub fn spawn(cmd: CoreCommand) -> AppResult<CoreProcess> {
    // §18: the core log is bounded exactly like the application log (10 MiB
    // per file, 7 days kept), so a chatty core cannot fill the disk.
    let writer = log_writer(&cmd.log_file)?;

    let mut child = tokio::process::Command::new(&cmd.exe)
        .arg("-d")
        .arg(&cmd.home_dir)
        .arg("-f")
        .arg(&cmd.config_file)
        .arg("-ext-ctl")
        .arg(cmd.controller.to_string())
        .arg("-secret")
        .arg(&cmd.secret)
        .current_dir(&cmd.home_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(NO_WINDOW | SUSPENDED)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AppError::new(AppErrorKind::CoreBinaryInvalid, STAGE, e.to_string()))?;

    // Both streams converge on one writer, which is why it is shared.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    if let Some(stream) = stdout.take() {
        pump(stream, writer.clone());
    }
    if let Some(stream) = stderr.take() {
        pump(stream, writer.clone());
    }

    let pid = child.id().unwrap_or_default();
    let raw = child.raw_handle().unwrap_or(std::ptr::null_mut());
    let job = match JobHandle::create().and_then(|job| job.assign(raw).map(|()| job)) {
        Ok(job) => job,
        Err(e) => {
            // Still suspended: kill_on_drop reaps it when `child` goes out of scope.
            let _ = child.start_kill();
            return Err(e);
        }
    };
    if let Err(e) = resume_threads(pid) {
        let _ = child.start_kill();
        return Err(e);
    }

    let (kill_tx, kill_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = watch::channel(None);
    tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status,
            _ = kill_rx => {
                let _ = child.start_kill();
                child.wait().await
            }
        };
        let code = status.ok().and_then(|s| s.code());
        let _ = exit_tx.send(Some(ExitInfo { code }));
    });

    Ok(CoreProcess {
        pid,
        _job: job,
        kill: Some(kill_tx),
        exit: exit_rx,
    })
}

/// Last lines of the core log, used to explain an early exit. `stem` is the
/// rolling writer's base path; the newest segment is read.
pub fn log_tail(stem: &std::path::Path, lines: usize) -> String {
    let path = crate::logging::newest_segment(stem);
    let text = std::fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join(" | ")
}

/// One rolling writer per core-log stem, so a restart keeps appending to the
/// same bounded file set instead of opening a new one each time.
fn log_writer(stem: &std::path::Path) -> AppResult<Arc<RollingWriter>> {
    let mut writers = LOG_WRITERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = writers.get(stem) {
        return Ok(existing.clone());
    }
    let writer = Arc::new(
        RollingWriter::open(stem.to_path_buf(), MAX_FILE_BYTES)
            .map_err(|e| AppError::storage(STAGE, e))?,
    );
    writers.insert(stem.to_path_buf(), writer.clone());
    Ok(writer)
}

static LOG_WRITERS: OnceLock<Mutex<HashMap<PathBuf, Arc<RollingWriter>>>> = OnceLock::new();

/// Copies one core stream into the rolling log until the pipe closes.
fn pump(
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    writer: Arc<RollingWriter>,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Err(e) = writer.append(&buf[..n]) {
                        tracing::debug!(error = %e, "core log write failed");
                        break;
                    }
                }
            }
        }
    });
}

struct JobHandle(HANDLE);

// SAFETY: a job object handle is a kernel handle usable from any thread.
unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

impl JobHandle {
    fn create() -> AppResult<Self> {
        // SAFETY: anonymous job creation; the limit struct is fully initialised
        // and its size is passed exactly.
        unsafe {
            let job = CreateJobObjectW(None, None).map_err(|e| AppError::internal(STAGE, e))?;
            let job = Self(job);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| AppError::internal(STAGE, e))?;
            Ok(job)
        }
    }

    fn assign(&self, process: *mut c_void) -> AppResult<()> {
        // SAFETY: `process` is the live handle owned by the tokio Child.
        unsafe { AssignProcessToJobObject(self.0, HANDLE(process)) }
            .map_err(|e| AppError::internal(STAGE, e))
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

/// Resumes every thread of a process created with CREATE_SUSPENDED.
fn resume_threads(pid: u32) -> AppResult<()> {
    // SAFETY: snapshot iteration with a correctly sized THREADENTRY32; every
    // opened handle is closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)
            .map_err(|e| AppError::internal(STAGE, e))?;
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
            return Err(AppError::internal(STAGE, "无法恢复内核主线程"));
        }
    }
    Ok(())
}
