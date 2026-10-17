//! Closed launcher boundary: direct user process or service-owned TUN process.
use super::{
    process::{CoreProcess, ExitInfo},
    service::{ServiceClient, ServiceProcess},
};
use std::time::Duration;
use tokio::sync::watch;

pub enum Process {
    Direct(CoreProcess),
    Service(ServiceProcess),
}
impl Process {
    pub fn pid(&self) -> u32 {
        match self {
            Self::Direct(p) => p.pid,
            Self::Service(p) => p.pid,
        }
    }
    pub fn service(&self) -> Option<ServiceClient> {
        match self {
            Self::Service(p) => Some(p.client.clone()),
            _ => None,
        }
    }
    pub fn exit_watch(&self) -> watch::Receiver<Option<ExitInfo>> {
        match self {
            Self::Direct(p) => p.exit_watch(),
            Self::Service(p) => p.exit_watch(),
        }
    }
    pub fn has_exited(&self) -> bool {
        match self {
            Self::Direct(p) => p.has_exited(),
            Self::Service(p) => p.has_exited(),
        }
    }
    pub async fn terminate(self, grace: Duration) -> bool {
        match self {
            Self::Direct(p) => {
                p.terminate(grace).await;
                true
            }
            Self::Service(p) => p.terminate(grace).await,
        }
    }
}
