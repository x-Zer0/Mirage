//! Application error model (DESIGN.md §5.3).

use super::redact::redact_text;
use std::fmt;

#[allow(dead_code)] // the full taxonomy is fixed by the design; later milestones use the rest
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppErrorKind {
    Network,
    Timeout,
    Http(u16),
    UnsupportedFormat,
    ResponseTooLarge,
    NotAConfig,
    ConfigCheckFailed,
    PolicyRejected,
    CoreBinaryInvalid,
    PortInUse,
    CoreExited,
    CoreNotReady,
    Storage,
    Cancelled,
    Internal,
}

impl AppErrorKind {
    /// Short user-facing label; the detail carries the specifics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Network => "网络错误",
            Self::Timeout => "请求超时",
            Self::Http(_) => "HTTP 错误",
            Self::UnsupportedFormat => "不支持的配置格式",
            Self::ResponseTooLarge => "内容过大",
            Self::NotAConfig => "不是有效的配置",
            Self::ConfigCheckFailed => "配置校验失败",
            Self::PolicyRejected => "配置包含不允许的设置",
            Self::CoreBinaryInvalid => "内核文件无效",
            Self::PortInUse => "端口被占用",
            Self::CoreExited => "内核已退出",
            Self::CoreNotReady => "内核未就绪",
            Self::Storage => "存储错误",
            Self::Cancelled => "已取消",
            Self::Internal => "内部错误",
        }
    }
}

/// Error with a redacted, displayable detail. Construct through [`AppError::new`]
/// so the detail is always redacted before it can reach a log or the UI.
#[derive(Debug, Clone)]
pub struct AppError {
    pub kind: AppErrorKind,
    pub stage: &'static str,
    detail: String,
}

impl AppError {
    pub fn new(kind: AppErrorKind, stage: &'static str, detail: impl AsRef<str>) -> Self {
        Self {
            kind,
            stage,
            detail: redact_text(detail.as_ref()).into_owned(),
        }
    }

    pub fn storage(stage: &'static str, err: impl fmt::Display) -> Self {
        Self::new(AppErrorKind::Storage, stage, err.to_string())
    }

    pub fn internal(stage: &'static str, err: impl fmt::Display) -> Self {
        Self::new(AppErrorKind::Internal, stage, err.to_string())
    }

    /// One line for the UI error bar.
    pub fn user_message(&self) -> String {
        if self.detail.is_empty() {
            self.kind.label().to_owned()
        } else {
            format!("{}：{}", self.kind.label(), self.detail)
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {:?}: {}", self.stage, self.kind, self.detail)
    }
}

impl std::error::Error for AppError {}

pub type AppResult<T> = Result<T, AppError>;
