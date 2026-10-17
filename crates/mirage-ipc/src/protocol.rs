//! Wire format between the app and `MirageSvc` (DESIGN.md §11.7).
//!
//! A frame is a 4-byte little-endian body length followed by one JSON
//! document. Requests never carry paths — only YAML content — so the service
//! has no way to read or execute a file the user can write.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{self, Read, Write};

pub const PIPE_NAME: &str = r"\\.\pipe\mirage-svc";
/// Incremented when `Request`/`Response` change incompatibly.
pub const PROTOCOL_VERSION: u32 = 2;
/// Both sides refuse anything larger; a run config is far below this.
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Must be the first frame on a connection.
    Hello {
        protocol: u32,
        app_version: String,
    },
    /// §11.7: content, not a path. The service re-applies the policy.
    StartCore {
        profile_id: String,
        config_yaml: String,
        ext_ctl_port: u16,
        secret: String,
    },
    /// Same policy and -t check as StartCore, then reload from the protected file.
    ReloadCore {
        config_yaml: String,
    },
    StopCore,
    Status,
    /// Kills a leftover core recorded by an abnormal exit (§11.5).
    Cleanup,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u32,
        service_version: String,
        status: ServiceStatus,
    },
    Started {
        pid: u32,
        status: ServiceStatus,
    },
    Stopped,
    Status(ServiceStatus),
    Ok,
    Err {
        kind: ErrorKind,
        detail: String,
    },
}

/// State of the core the service owns; `Status` is polled while TUN is on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ServiceStatus {
    pub running: bool,
    pub pid: u32,
    /// Exit code of the last core the service ran, if it has ended.
    pub last_exit: Option<i32>,
    /// Unix seconds when the running core was started.
    pub started_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Protocol or service version differs from the app's (§11.7 upgrade).
    VersionMismatch,
    /// The YAML failed the §9.3/§9.4 re-check.
    PolicyRejected,
    /// `mihomo -t` failed.
    CheckFailed,
    /// `StopCore`/`Status` with nothing running.
    CoreNotRunning,
    /// `StartCore` while the previous core is still running.
    AlreadyRunning,
    /// Another request is in flight.
    Busy,
    Io,
}

impl ErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::VersionMismatch => "辅助服务版本不一致，请更新服务",
            Self::PolicyRejected => "配置包含不允许的设置",
            Self::CheckFailed => "配置校验失败",
            Self::CoreNotRunning => "辅助服务中没有运行中的内核",
            Self::AlreadyRunning => "辅助服务中已有内核在运行",
            Self::Busy => "辅助服务正忙",
            Self::Io => "辅助服务读写失败",
        }
    }
}

#[derive(Debug)]
pub enum FrameError {
    /// The peer closed the connection between frames.
    Eof,
    Io(io::Error),
    /// Not JSON, or larger than [`MAX_FRAME_BYTES`].
    Malformed(String),
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub type FrameResult<T> = Result<T, FrameError>;

/// Writes one frame and flushes it (a pipe write is not buffered by us).
pub fn write_frame<T: Serialize>(out: &mut impl Write, value: &T) -> FrameResult<()> {
    let body =
        serde_json::to_vec(value).map_err(|e| FrameError::Malformed(format!("序列化失败：{e}")))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Malformed("帧超过大小上限".into()));
    }
    let len = u32::try_from(body.len())
        .map_err(|_| FrameError::Malformed("帧长度溢出".into()))?
        .to_le_bytes();
    out.write_all(&len)?;
    out.write_all(&body)?;
    out.flush()?;
    Ok(())
}

/// Reads one frame; [`FrameError::Eof`] when the peer disconnected cleanly.
pub fn read_frame<T: DeserializeOwned>(input: &mut impl Read) -> FrameResult<T> {
    let mut header = [0u8; 4];
    if !read_exact_or_eof(input, &mut header)? {
        return Err(FrameError::Eof);
    }
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(FrameError::Malformed(format!("帧长度 {len} 不合法")));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| FrameError::Malformed(format!("JSON 解析失败：{e}")))
}

/// `Ok(false)` when the stream ended before the first byte of a frame.
fn read_exact_or_eof(input: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => {
                return if filled == 0 {
                    Ok(false)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "帧在长度头中途结束",
                    ))
                };
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Async transport used by both named-pipe endpoints. Cancellation drops the
/// whole connection; a partial frame is never reused.
pub async fn send<T: Serialize>(
    out: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
) -> FrameResult<()> {
    use tokio::io::AsyncWriteExt;
    let body = serde_json::to_vec(value).map_err(|e| FrameError::Malformed(e.to_string()))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Malformed("帧过大".into()));
    }
    out.write_all(&(body.len() as u32).to_le_bytes()).await?;
    out.write_all(&body).await?;
    out.flush().await?;
    Ok(())
}

pub async fn receive<T: DeserializeOwned>(
    input: &mut (impl tokio::io::AsyncRead + Unpin),
) -> FrameResult<T> {
    use tokio::io::AsyncReadExt;
    let len = input.read_u32_le().await? as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(FrameError::Malformed("帧长度不合法".into()));
    }
    let mut body = vec![0; len];
    input.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|e| FrameError::Malformed(e.to_string()))
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => f.write_str("管道已关闭"),
            Self::Io(e) => write!(f, "管道读写失败：{e}"),
            Self::Malformed(detail) => write!(f, "管道帧格式不合法：{detail}"),
        }
    }
}
impl std::error::Error for FrameError {}
