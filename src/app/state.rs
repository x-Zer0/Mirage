//! Snapshot published to the UI (DESIGN.md §6.5 `Event::State`).

use crate::cores::mihomo::controller::ProxyGroupView;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RuntimeStatus {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileView {
    pub id: String,
    pub name: String,
    pub source: String,
    pub detail: String,
    pub updated: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub error: bool,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct AppStateView {
    pub core_version: String,
    pub core_ready: bool,
    pub runtime: RuntimeStatus,
    pub mixed_port: u16,
    pub profiles: Vec<ProfileView>,
    pub selected_profile: String,
    pub groups: Vec<ProxyGroupView>,
    pub testing_group: Option<String>,
    pub busy: Option<String>,
    pub notice: Option<Notice>,
    pub policy_notes: Vec<String>,
    pub check_lines: Vec<String>,
}
