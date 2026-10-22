//! Commands from the UI and completions from background tasks.
use super::runtime::Running;
use super::{diagnostics, pipeline, runtime, updates};
use crate::cores::mihomo::{binary::CoreBinary, controller::ProxyGroupView};
use crate::domain::error::AppError;
use crate::domain::model::{AccountId, ProfileId, RevisionId, RoutingMode};
use crate::domain::redact::{Password, SecretString};

#[derive(Debug)]
pub enum Command {
    QueryLogs(super::logs::LogQuery),
    ClearConnectionLogs,
    PickAndImport,
    /// From `--import <path>`; queued until the core binary is verified.
    ImportLocalFile(std::path::PathBuf),
    SelectProfile(String),
    RemoveProfile(String),
    /// Name may be empty (then content-disposition or the host is used).
    AddRemoteProfile {
        name: String,
        url: SecretString,
    },
    UpdateProfile(String),
    UpdateAll,
    /// ziyoumao login; the password is zeroed after the request (§6.1).
    ProviderLogin {
        provider_id: String,
        email: String,
        password: Password,
    },
    RevalidateAccount(AccountId),
    RevealAccount {
        id: AccountId,
        visible: bool,
    },
    /// Read-only import of the original client's config (ZIYOUMAO.md §9).
    ImportLegacyProvider(String),
    /// Diagnostics: bootstrap + host checks without a token.
    ProbeProviderHosts,
    Connect,
    Disconnect,
    SelectNode {
        group: String,
        node: String,
    },
    TestGroupDelay(String),
    SetSystemProxy(bool),
    SetTun(bool),
    ManageService(bool),
    CheckConnectivity,
    DismissNotice,
    /// Error bar retry button: re-sends the failed operation (§17.3).
    RetryNotice,
    SetRoutingMode(RoutingMode),
    SaveSettings(super::settings::SettingsInput),
    /// Rules page "保存": enable switch and full text (YAML or hosts format).
    SaveOverride {
        kind: super::overrides::OverrideKind,
        enabled: bool,
        text: String,
    },
    SetUiGeometry(crate::storage::meta::UiGeometry),
    /// Interval (hours, 0 = manual) and egress index for one profile.
    SetUpdatePolicy {
        id: String,
        interval_hours: u32,
        egress: usize,
    },
    /// Copies a URL subscription's link to the clipboard on user request.
    CopyProfileUrl(String),
    UpdateGeodata,
    /// Traffic redraws follow visibility; connection collection stays active.
    WindowVisible(bool),
    CloseHintShown,
    /// Writes the diagnostics bundle to a user-chosen file (§18).
    ExportDiagnostics,
    /// Installs the receiver of visible-window traffic samples.
    SetLiveSink(std::sync::Arc<dyn super::live::LiveSink>),
    /// Logoff/shutdown: clean up now and acknowledge when done (§11.5).
    SessionEnding(std::sync::mpsc::Sender<()>),
    Quit,
}

pub(super) enum Internal {
    Connections(u64, Vec<super::logs::ConnectionLog>),
    LogStreamStatus(u64, String),
    NodeSelected {
        generation: u64,
        group: String,
        node: String,
        result: Result<(), AppError>,
    },
    ServiceManaged {
        generation: u64,
        install: bool,
        enable: bool,
        result: Result<(), AppError>,
    },
    CoreVerified(Result<CoreBinary, AppError>),
    Imported(String, Result<pipeline::ImportOutcome, AppError>),
    Started(u64, Result<Running, AppError>),
    CoreExited {
        generation: u64,
        detail: String,
    },
    Groups(u64, u64, Result<Vec<ProxyGroupView>, AppError>),
    Stopped(u64),
    NodeDelay(u64, u64, super::delays::DelayEvent),
    RestartDue(u64),
    Connectivity(u64, u64, Result<u32, AppError>),
    RemoteAdded(Box<updates::AddedRemote>),
    /// `bool`: started by the user (notices) rather than the schedule.
    Updated(Box<pipeline::UpdateOutcome>, bool),
    Reloaded {
        generation: u64,
        profile_id: ProfileId,
        revision: RevisionId,
        outcome: runtime::ReloadOutcome,
    },
    OverrideChecked {
        kind: super::overrides::OverrideKind,
        enabled: bool,
        text: String,
        result: super::overrides::CheckResult,
    },
    OverlayApplied {
        generation: u64,
        attempted: crate::cores::mihomo::compose::UserOverlay,
        previous: crate::cores::mihomo::compose::UserOverlay,
        outcome: runtime::OverlayOutcome,
    },
    LoggedIn {
        provider_id: String,
        result: Box<Result<crate::providers::LoggedIn, AppError>>,
    },
    ProviderProfileAdded {
        account_id: AccountId,
        result: Box<Result<pipeline::RemoteAdded, AppError>>,
    },
    AccountRefreshed {
        account_id: AccountId,
        result: Result<crate::providers::Refreshed, AppError>,
        /// Started by the user (notices) rather than a background check.
        manual: bool,
    },
    HostsProbed(Result<crate::providers::ziyoumao::bootstrap::Discovery, AppError>),
    ModeApplied(u64, RoutingMode, Result<(), AppError>),
    CoreMode(u64, Result<String, AppError>),
    GeoUpdated(Result<(), AppError>),
    /// Diagnostics bundle written (or the user cancelled the save dialog).
    DiagnosticsExported(Result<Option<diagnostics::Bundle>, AppError>),
}
