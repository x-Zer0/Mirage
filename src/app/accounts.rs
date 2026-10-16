//! Provider accounts in the actor (DESIGN.md §16.3, ZIYOUMAO.md §6–§7):
//! login, background session checks, removal, and resolving the
//! subscription of an account-backed profile before each update.
//!
//! Account state is independent of the core: a failed check never stops
//! the core or drops the profile's working revision (§5.1).

use super::pipeline::{self, RemoteAdded, RemoteFetch};
use super::{AppCore, Internal, error_notice, info_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{
    AccountId, AccountStatus, Profile, ProviderAccount, SubscriptionSource, UpdatePolicy, new_id,
    now,
};
use crate::domain::redact::{Password, SecretString};
use crate::providers::ziyoumao::{self, Egress, LoggedIn, Refreshed, bootstrap};
use crate::storage::layout::Layout;
use crate::storage::meta::{self, AccountsFile};
use crate::subscriptions::fetch::Descriptor;
use tokio::sync::mpsc;

const STAGE: &str = "app.accounts";
pub const PROFILE_NAME: &str = "自由猫";
pub const LEGACY_PROFILE_NAME: &str = "自由猫导入";

pub struct Accounts {
    file: AccountsFile,
}

impl Accounts {
    pub fn load(layout: &Layout) -> AppResult<Self> {
        let mut file: AccountsFile = meta::load(&layout.accounts_file(), STAGE)?;
        // Transient states from a previous run mean nothing now.
        for a in &mut file.accounts {
            if matches!(
                a.status,
                AccountStatus::LoggingIn | AccountStatus::Validating
            ) {
                a.status = AccountStatus::Valid;
            }
        }
        Ok(Self { file })
    }

    pub fn empty() -> Self {
        Self {
            file: AccountsFile::default(),
        }
    }

    pub fn all(&self) -> &[ProviderAccount] {
        &self.file.accounts
    }

    pub fn get(&self, id: &str) -> Option<&ProviderAccount> {
        self.file.accounts.iter().find(|a| a.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut ProviderAccount> {
        self.file.accounts.iter_mut().find(|a| a.id == id)
    }

    /// v1 UI shows one account per provider (ZIYOUMAO.md §3).
    fn by_provider(&self, provider: &str) -> Option<&ProviderAccount> {
        self.file
            .accounts
            .iter()
            .find(|a| a.provider_id == provider)
    }

    fn save(&mut self, layout: &Layout) -> AppResult<()> {
        meta::save(&layout.accounts_file(), &mut self.file, STAGE)
    }
}

/// What an update of an account-backed profile needs, read in the actor.
pub struct ProviderPlan {
    pub account_id: AccountId,
    trusted_hosts: Vec<String>,
    session: SecretString,
    cached_url: Option<SecretString>,
}

/// §7.2: fetch the latest subscription URL first; on a network failure use
/// the saved one; on an auth failure stop and ask for a new login.
pub async fn resolve(
    plan: ProviderPlan,
    layout: &Layout,
    egress: Egress,
    tx: &mpsc::UnboundedSender<Internal>,
) -> AppResult<Descriptor> {
    let refreshed =
        ziyoumao::refresh(layout, &plan.trusted_hosts, plan.session.expose(), egress).await;
    let fallback = match &refreshed {
        Ok(r) => Ok(r.subscribe_url.clone()),
        Err(e) if ziyoumao::is_unreachable(e) => match &plan.cached_url {
            Some(url) => {
                tracing::info!(error = %e, "using cached subscription url");
                Ok(url.clone())
            }
            None => Err(e.clone()),
        },
        Err(e) => Err(e.clone()),
    };
    let _ = tx.send(Internal::AccountRefreshed {
        account_id: plan.account_id,
        result: refreshed,
        manual: false,
    });
    ziyoumao::descriptor(&fallback?)
}

impl AppCore {
    pub(super) fn provider_egress(&self) -> Egress {
        Egress {
            policy: self.settings.provider_egress,
            core_port: self.core_port(),
        }
    }

    pub(super) fn provider_login(&mut self, email: String, password: Password) {
        if self.view.login_busy {
            return;
        }
        self.view.login_busy = true;
        self.view.login_message = None;
        let layout = self.layout.clone();
        let egress = self.provider_egress();
        self.spawn(async move {
            Internal::LoggedIn(Box::new(
                ziyoumao::login(&layout, email, password, egress).await,
            ))
        });
    }

    pub(super) fn on_logged_in(&mut self, result: AppResult<LoggedIn>) {
        self.view.login_busy = false;
        let logged_in = match result {
            Ok(l) => l,
            Err(e) => return self.on_login_failed(e),
        };
        // Re-login of the existing account keeps its id (and profile).
        let id = self
            .accounts
            .by_provider(ziyoumao::PROVIDER_ID)
            .map(|a| a.id.clone())
            .unwrap_or_else(new_id);
        let session_key = format!("account/{id}/session");
        if let Err(e) = self
            .secrets
            .put(&session_key, logged_in.session.expose().as_bytes())
        {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        let url_key = format!("account/{id}/subscribe_url");
        if let Some(url) = &logged_in.subscribe_url
            && let Err(e) = self.secrets.put(&url_key, url.expose().as_bytes())
        {
            tracing::warn!(error = %e, "subscribe url not stored");
        }
        let existing = self.accounts.get(&id).cloned();
        let account = ProviderAccount {
            id: id.clone(),
            provider_id: ziyoumao::PROVIDER_ID.into(),
            display_name: logged_in.email_masked.clone(),
            session_secret: Some(session_key),
            subscribe_secret: Some(url_key),
            // §4.3: the login host joins the trusted list (tried first);
            // hosts only ever join through a login.
            trusted_hosts: std::iter::once(logged_in.trusted_host.clone())
                .chain(
                    existing
                        .iter()
                        .flat_map(|a| a.trusted_hosts.iter().cloned())
                        .filter(|h| *h != logged_in.trusted_host),
                )
                .collect(),
            status: AccountStatus::Valid,
            snapshot: logged_in
                .snapshot
                .clone()
                .or_else(|| existing.as_ref().and_then(|a| a.snapshot.clone())),
            profile_id: existing.as_ref().and_then(|a| a.profile_id.clone()),
            created_at: existing.as_ref().map_or_else(now, |a| a.created_at),
        };
        match self.accounts.get_mut(&id) {
            Some(slot) => *slot = account,
            None => self.accounts.file.accounts.push(account),
        }
        self.save_accounts();
        tracing::info!(account = %id, "provider account logged in");
        if let Some(e) = logged_in.follow_up_error {
            self.view.notice = Some(Notice::error(format!(
                "已登录，但读取账号信息失败：{}",
                e.user_message()
            )));
        } else {
            self.view.notice = Some(info_notice(format!(
                "已登录自由猫账号 {}",
                logged_in.email_masked
            )));
        }
        self.sync_account_usage(&id);
        self.ensure_account_profile(&id, logged_in.subscribe_url);
        self.refresh_accounts_view();
    }

    fn on_login_failed(&mut self, e: AppError) {
        tracing::warn!(error = %e, "provider login failed");
        let text = e.user_message();
        if e.kind == AppErrorKind::NeedsUserAction
            && let Some(id) = self
                .accounts
                .by_provider(ziyoumao::PROVIDER_ID)
                .map(|a| a.id.clone())
            && let Some(a) = self.accounts.get_mut(&id)
        {
            a.status = AccountStatus::NeedsUserAction(text.clone());
            self.save_accounts();
        }
        // An existing session survives a failed re-login (§6.4).
        self.view.login_message = Some(text);
        self.refresh_accounts_view();
    }

    /// Creates the account's profile on first login; later logins update it.
    fn ensure_account_profile(&mut self, account_id: &str, url: Option<SecretString>) {
        let profile_id = self
            .accounts
            .get(account_id)
            .and_then(|a| a.profile_id.clone())
            .filter(|p| self.profiles.get(p).is_some());
        if let Some(profile_id) = profile_id {
            self.update_profile(&profile_id, true);
            return;
        }
        let Some(url) = url else { return };
        let desc = match ziyoumao::descriptor(&url) {
            Ok(d) => d,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        let Some(binary) = self.require_binary() else {
            return;
        };
        let req = RemoteFetch {
            binary,
            layout: self.layout.clone(),
            desc,
            egress: UpdatePolicy::default().egress,
            core_port: self.core_port(),
            mixed_port: self.view.mixed_port,
        };
        self.view.busy = Some("正在下载并校验自由猫订阅…".into());
        let permits = self.fetch_permits.clone();
        let account_id = account_id.to_owned();
        self.spawn(async move {
            let _permit = permits.acquire_owned().await;
            Internal::ProviderProfileAdded {
                account_id,
                result: Box::new(pipeline::add_remote(req).await),
            }
        });
    }

    pub(super) fn on_provider_profile_added(
        &mut self,
        account_id: AccountId,
        result: AppResult<RemoteAdded>,
    ) {
        self.view.busy = None;
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                self.view.notice = Some(Notice::error(format!(
                    "自由猫订阅未能导入：{}",
                    e.user_message()
                )));
                return;
            }
        };
        let source = SubscriptionSource::ProviderAccount {
            provider_id: ziyoumao::PROVIDER_ID.into(),
            account_id: account_id.clone(),
        };
        let profile_id = outcome.profile_id.clone();
        if self.store_new_profile(outcome, PROFILE_NAME.into(), source) {
            if let Some(a) = self.accounts.get_mut(&account_id) {
                a.profile_id = Some(profile_id);
            }
            self.save_accounts();
            self.sync_account_usage(&account_id);
            self.refresh_accounts_view();
        }
    }

    pub(super) fn revalidate_account(&mut self, id: &str, manual: bool) {
        let Some(account) = self.accounts.get(id).cloned() else {
            return;
        };
        if account.status == AccountStatus::Validating {
            return;
        }
        let Some(session) = self.load_session(&account) else {
            if manual {
                self.view.notice = Some(info_notice("该账号没有已保存的会话，请重新登录"));
            }
            return;
        };
        let previous = account.status.clone();
        if let Some(a) = self.accounts.get_mut(id) {
            a.status = AccountStatus::Validating;
        }
        self.validating_prev.insert(id.to_owned(), previous);
        let layout = self.layout.clone();
        let egress = self.provider_egress();
        let id = id.to_owned();
        self.spawn(async move {
            let result =
                ziyoumao::refresh(&layout, &account.trusted_hosts, session.expose(), egress).await;
            Internal::AccountRefreshed {
                account_id: id,
                result,
                manual,
            }
        });
        self.refresh_accounts_view();
    }

    /// Applies a session check (§6.4): only an auth failure needs a new
    /// login; network trouble keeps the token and is shown as unavailable.
    pub(super) fn on_account_refreshed(
        &mut self,
        id: AccountId,
        result: AppResult<Refreshed>,
        manual: bool,
    ) {
        let previous = self.validating_prev.remove(&id);
        let Some(account) = self.accounts.get_mut(&id) else {
            return;
        };
        let mut new_url = None;
        match result {
            Ok(r) => {
                account.status = AccountStatus::Valid;
                account.snapshot = Some(r.snapshot);
                new_url = Some(r.subscribe_url);
                if manual {
                    self.view.notice = Some(info_notice("账号信息已刷新"));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, account = %id, "account check failed");
                account.status = match e.kind {
                    AppErrorKind::AuthInvalid => AccountStatus::ReauthRequired,
                    _ if ziyoumao::is_unreachable(&e) => {
                        AccountStatus::NetworkUnavailable(e.user_message())
                    }
                    _ => previous
                        .filter(|p| *p != AccountStatus::Validating)
                        .unwrap_or(AccountStatus::Valid),
                };
                if manual || e.kind == AppErrorKind::AuthInvalid {
                    let notice = Notice::error(e.user_message()).with_stage("自由猫账号");
                    self.view.notice = Some(if e.kind == AppErrorKind::AuthInvalid {
                        notice.notifying()
                    } else {
                        notice.with_retry(Retry::RevalidateAccount(id.clone()))
                    });
                }
            }
        }
        let url_key = account.subscribe_secret.clone();
        self.save_accounts();
        if let (Some(url), Some(key)) = (new_url, url_key) {
            self.store_subscribe_url(&key, &url);
        }
        self.sync_account_usage(&id);
        self.refresh_accounts_view();
    }

    /// §7.2: the token in the URL may rotate; keep the newest one.
    fn store_subscribe_url(&self, key: &str, url: &SecretString) {
        let current = self.secrets.get(key).ok().flatten();
        if current.as_deref() == Some(url.expose().as_bytes()) {
            return;
        }
        tracing::info!("subscription url changed, stored the new one");
        if let Err(e) = self.secrets.put(key, url.expose().as_bytes()) {
            tracing::warn!(error = %e, "subscribe url not stored");
        }
    }

    /// Local-only removal (§6.4): tokens, URL and metadata; the profile is
    /// removed too only when the user asked for it.
    pub(super) fn remove_account(&mut self, id: &str, with_profile: bool) {
        let Some(index) = self.accounts.file.accounts.iter().position(|a| a.id == id) else {
            return;
        };
        let account = self.accounts.file.accounts.remove(index);
        for key in [&account.session_secret, &account.subscribe_secret]
            .into_iter()
            .flatten()
        {
            if let Err(e) = self.secrets.delete(key) {
                tracing::warn!(error = %e, "account secret not deleted");
            }
        }
        self.save_accounts();
        if with_profile && let Some(profile_id) = &account.profile_id {
            self.remove_profile(profile_id);
        }
        self.view.notice = Some(info_notice("已移除自由猫账号（仅清除本机数据）"));
        self.refresh_accounts_view();
    }

    /// Plan for updating an account-backed profile; errors explain why not.
    pub(super) fn provider_plan(&self, account_id: &str) -> AppResult<ProviderPlan> {
        let account = self.accounts.get(account_id).ok_or_else(|| {
            AppError::new(
                AppErrorKind::AuthInvalid,
                STAGE,
                "关联的自由猫账号已移除，请重新登录",
            )
        })?;
        if account.status == AccountStatus::ReauthRequired {
            return Err(AppError::new(
                AppErrorKind::AuthInvalid,
                STAGE,
                "自由猫登录已失效，请在“账号”页重新登录",
            ));
        }
        let session = self.load_session(account).ok_or_else(|| {
            AppError::new(
                AppErrorKind::AuthInvalid,
                STAGE,
                "没有已保存的会话，请重新登录",
            )
        })?;
        let cached_url = account
            .subscribe_secret
            .as_ref()
            .and_then(|k| self.secrets.get(k).ok().flatten())
            .and_then(|b| String::from_utf8(b).ok())
            .map(SecretString::new);
        Ok(ProviderPlan {
            account_id: account.id.clone(),
            trusted_hosts: account.trusted_hosts.clone(),
            session,
            cached_url,
        })
    }

    fn load_session(&self, account: &ProviderAccount) -> Option<SecretString> {
        let key = account.session_secret.as_ref()?;
        match self.secrets.get(key) {
            Ok(Some(bytes)) => String::from_utf8(bytes).ok().map(SecretString::new),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "session not readable");
                None
            }
        }
    }

    /// Provider usage wins over subscription headers (§15).
    fn sync_account_usage(&mut self, account_id: &str) {
        let Some(account) = self.accounts.get(account_id) else {
            return;
        };
        let (Some(profile_id), Some(usage)) = (
            account.profile_id.clone(),
            account.snapshot.as_ref().and_then(|s| s.usage()),
        ) else {
            return;
        };
        if let Some(p) = self.profiles.get_mut(&profile_id)
            && p.usage != Some(usage)
        {
            p.usage = Some(usage);
            self.save_profiles();
            self.refresh_profiles_view();
        }
    }

    /// Background session check of every saved account (§16.1, §6.4).
    pub(super) fn validate_accounts_on_start(&mut self) {
        let ids: Vec<AccountId> = self
            .accounts
            .all()
            .iter()
            .filter(|a| a.session_secret.is_some() && a.status != AccountStatus::ReauthRequired)
            .map(|a| a.id.clone())
            .collect();
        for id in ids {
            self.revalidate_account(&id, false);
        }
    }

    /// `profile_id` is no longer the account's profile.
    pub(super) fn unlink_profile(&mut self, profile_id: &str) {
        let mut changed = false;
        for a in &mut self.accounts.file.accounts {
            if a.profile_id.as_deref() == Some(profile_id) {
                a.profile_id = None;
                changed = true;
            }
        }
        if changed {
            self.save_accounts();
        }
    }

    pub(super) fn import_legacy(&mut self) {
        let Some(binary) = self.require_binary() else {
            return;
        };
        let layout = self.layout.clone();
        let port = self.view.mixed_port;
        self.view.busy = Some("正在导入原自由猫客户端配置…".into());
        self.spawn(async move {
            let result = async {
                let path = ziyoumao::legacy_import::default_path()?;
                let raw = ziyoumao::legacy_import::read(&path)?;
                pipeline::import_raw(&binary, &layout, LEGACY_PROFILE_NAME.into(), raw, port).await
            }
            .await;
            Internal::Imported(result)
        });
    }

    /// Diagnostics (ZIYOUMAO.md §10 step 1): candidate hosts and results.
    pub(super) fn probe_provider_hosts(&mut self) {
        if self.view.probing_hosts {
            return;
        }
        self.view.probing_hosts = true;
        let layout = self.layout.clone();
        let egress = self.provider_egress();
        self.spawn(
            async move { Internal::HostsProbed(bootstrap::discover(&layout, egress).await) },
        );
    }

    pub(super) fn on_hosts_probed(&mut self, result: AppResult<bootstrap::Discovery>) {
        self.view.probing_hosts = false;
        self.view.provider_hosts = match result {
            Ok(d) => {
                let mut lines = vec![if d.from_cache {
                    "入口文档不可用，使用缓存的主机列表".to_owned()
                } else {
                    "入口文档：已读取".to_owned()
                }];
                lines.extend(d.probes.iter().map(|p| match &p.result {
                    Ok(()) => format!("{}：可用", p.host),
                    Err(e) => format!("{}：{e}", p.host),
                }));
                lines.push(match &d.selected {
                    Some((base, _)) => format!("选用：{}", bootstrap::origin(base)),
                    None => "没有可用的主机".into(),
                });
                lines
            }
            Err(e) => vec![format!("主机发现失败：{}", e.user_message())],
        };
    }

    fn save_accounts(&mut self) {
        if let Err(e) = self.accounts.save(&self.layout) {
            self.view.notice = Some(error_notice(&e));
        }
    }

    pub(super) fn refresh_accounts_view(&mut self) {
        self.view.accounts = self
            .accounts
            .all()
            .iter()
            .map(super::state::AccountView::from_account)
            .collect();
    }
}

use super::state::{Notice, Retry};

/// Whether a profile belongs to a provider account.
pub fn account_of(profile: &Profile) -> Option<&str> {
    match &profile.source {
        SubscriptionSource::ProviderAccount { account_id, .. } => Some(account_id),
        _ => None,
    }
}
