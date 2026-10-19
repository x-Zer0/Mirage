//! Persistent authenticated service session. Its lifetime owns the TUN core.
use super::process::ExitInfo;
use crate::{
    domain::error::{AppError, AppErrorKind, AppResult},
    platform::windows::service,
    storage::atomic,
};
use mirage_ipc::protocol::{self, ErrorKind, PIPE_NAME, PROTOCOL_VERSION, Request, Response};
use std::{
    os::windows::io::AsRawHandle,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::windows::named_pipe::{ClientOptions, NamedPipeClient},
    sync::{mpsc, oneshot, watch},
};
use windows::Win32::{Foundation::HANDLE, System::Pipes::GetNamedPipeServerProcessId};

const STAGE: &str = "mihomo.service";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(95);

pub struct ServiceProcess {
    pub pid: u32,
    pub client: ServiceClient,
    stop: Option<oneshot::Sender<()>>,
    exit: watch::Receiver<Option<ExitInfo>>,
    cleaned: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct ServiceClient {
    tx: mpsc::Sender<Call>,
}
struct Call {
    request: Request,
    reply: oneshot::Sender<AppResult<Response>>,
}

impl ServiceProcess {
    pub async fn start(
        yaml: String,
        profile: String,
        port: u16,
        secret: String,
        journal_path: &std::path::Path,
    ) -> AppResult<Self> {
        let mut pipe = connect(true).await?;
        // Only StartCore can create a TUN core; failed authentication cannot.
        atomic::write(journal_path, br#"{"schema_version":1}"#)
            .map_err(|e| AppError::storage(STAGE, e))?;
        let response = exchange(
            &mut pipe,
            &Request::StartCore {
                config_yaml: yaml,
                profile_id: profile,
                ext_ctl_port: port,
                secret,
            },
        )
        .await?;
        let Response::Started { pid, .. } = response else {
            return Err(error("服务未返回内核进程"));
        };
        let (tx, rx) = mpsc::channel(4);
        let (stop, stopped) = oneshot::channel();
        let (exit_tx, exit) = watch::channel(None);
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup = cleaned.clone();
        tokio::spawn(async move {
            let code = own_session(pipe, rx, stopped, pid, cleanup).await;
            let _ = exit_tx.send(Some(ExitInfo { code }));
        });
        Ok(Self {
            pid,
            client: ServiceClient { tx },
            stop: Some(stop),
            exit,
            cleaned,
        })
    }
    pub fn exit_watch(&self) -> watch::Receiver<Option<ExitInfo>> {
        self.exit.clone()
    }
    pub fn has_exited(&self) -> bool {
        self.exit.borrow().is_some()
    }
    pub async fn terminate(mut self, grace: Duration) -> bool {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(grace, self.exit.wait_for(Option::is_some)).await;
        self.cleaned.load(Ordering::SeqCst)
    }
}
impl ServiceClient {
    pub async fn reload(&self, yaml: String) -> AppResult<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Call {
                request: Request::ReloadCore { config_yaml: yaml },
                reply,
            })
            .await
            .map_err(|_| error("TUN 会话已结束"))?;
        match rx.await.map_err(|_| error("TUN 会话已结束"))?? {
            Response::Ok => Ok(()),
            _ => Err(error("服务返回了意外的重载结果")),
        }
    }
}

async fn own_session(
    mut pipe: NamedPipeClient,
    mut calls: mpsc::Receiver<Call>,
    mut stop: oneshot::Receiver<()>,
    pid: u32,
    cleaned: Arc<AtomicBool>,
) -> Option<i32> {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => {
                let ack = tokio::time::timeout(Duration::from_secs(3),
                    exchange(&mut pipe, &Request::StopCore)).await;
                cleaned.store(matches!(ack, Ok(Ok(Response::Stopped))), Ordering::SeqCst);
                return None;
            }
            call = calls.recv() => {
                let Some(call) = call else {
                    // 调用通道关闭，主动发 StopCore
                    let ack = tokio::time::timeout(Duration::from_secs(3),
                        exchange(&mut pipe, &Request::StopCore)).await;
                    cleaned.store(matches!(ack, Ok(Ok(Response::Stopped))), Ordering::SeqCst);
                    return None;
                };
                let result = tokio::select! {
                    biased;
                    _ = &mut stop => return None, // dropping pipe revokes ownership
                    result = exchange(&mut pipe, &call.request) => result,
                };
                // A transport error invalidates framing. Policy errors are recoverable.
                let broken = result.as_ref().is_err_and(|e| e.kind == AppErrorKind::ServiceUnavailable);
                let _ = call.reply.send(result);
                if broken { return None; }
            }
            _ = tick.tick() => {
                match exchange(&mut pipe, &Request::Status).await {
                    Ok(Response::Status(status)) if status.running && status.pid == pid => {}
                    Ok(Response::Status(status)) => { cleaned.store(!status.running, Ordering::SeqCst); return status.last_exit; },
                    _ => return None,
                }
            }
        }
    }
}

/// Checks the authenticated session without starting a core or changing routes.
pub async fn check() -> AppResult<()> {
    let mut pipe = connect(true).await?;
    match exchange(&mut pipe, &Request::Status).await? {
        Response::Status(_) => Ok(()),
        _ => Err(error("服务未返回内核状态")),
    }
}
pub async fn cleanup() -> AppResult<()> {
    let mut pipe = connect(false).await?;
    match exchange(&mut pipe, &Request::Cleanup).await? {
        Response::Ok => Ok(()),
        _ => Err(error("服务清理失败")),
    }
}

async fn connect(require_version: bool) -> AppResult<NamedPipeClient> {
    service::start().map_err(|e| error(format!("辅助服务不可用，请在设置中安装/更新服务：{e}")))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut pipe = loop {
        match ClientOptions::new().open(PIPE_NAME) {
            Ok(pipe) => break pipe,
            Err(e) if tokio::time::Instant::now() >= deadline => return Err(error(e)),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    };
    verify_server(&pipe)?;
    let response = exchange(
        &mut pipe,
        &Request::Hello {
            protocol: PROTOCOL_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await?;
    match response {
        Response::Hello {
            protocol,
            service_version,
            ..
        } if !require_version
            || (protocol == PROTOCOL_VERSION && service_version == env!("CARGO_PKG_VERSION")) =>
        {
            Ok(pipe)
        }
        Response::Hello { .. } => Err(error("辅助服务版本不一致，请在设置中更新服务")),
        _ => Err(error("辅助服务握手失败")),
    }
}

fn verify_server(pipe: &NamedPipeClient) -> AppResult<()> {
    let mut pid = 0;
    // SAFETY: connected client handle with a valid PID output pointer.
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(error)?;
    service::verify_process(pid).map_err(|e| error(format!("辅助服务身份校验失败：{e}")))
}

async fn exchange(pipe: &mut NamedPipeClient, request: &Request) -> AppResult<Response> {
    let operation = async {
        protocol::send(pipe, request).await?;
        protocol::receive(pipe).await
    };
    let timeout = if matches!(
        request,
        Request::StartCore { .. } | Request::ReloadCore { .. }
    ) {
        REQUEST_TIMEOUT
    } else {
        Duration::from_secs(5)
    };
    let response = tokio::time::timeout(timeout, operation)
        .await
        .map_err(|_| error("辅助服务响应超时"))?
        .map_err(error)?;
    if let Response::Err { kind, detail } = response {
        let mapped = match kind {
            ErrorKind::PolicyRejected => AppErrorKind::PolicyRejected,
            ErrorKind::CheckFailed => AppErrorKind::ConfigCheckFailed,
            _ => AppErrorKind::Internal,
        };
        return Err(AppError::new(
            mapped,
            STAGE,
            format!("{}：{detail}", kind.label()),
        ));
    }
    Ok(response)
}
fn error(detail: impl std::fmt::Display) -> AppError {
    AppError::new(AppErrorKind::ServiceUnavailable, STAGE, detail.to_string())
}
