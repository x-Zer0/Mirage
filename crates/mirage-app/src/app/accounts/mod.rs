//! Stored provider accounts, secrets and subscription resolution.
mod discovery;
mod login;
mod profiles;
mod session;

use super::{AppCore, Internal, error_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{
    AccountId, AccountStatus, Profile, ProviderAccount, SubscriptionSource,
};
use crate::domain::redact::SecretString;
use crate::providers::{self, Egress, account_provider};
use crate::storage::{
    layout::Layout,
    meta::{self, AccountsFile},
};
use crate::subscriptions::fetch::Descriptor;
use tokio::sync::mpsc;

const STAGE: &str = "app.accounts";

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
    provider_id: String,
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
    let provider = account_provider(&plan.provider_id)?;
    let refreshed = provider
        .refresh(layout, &plan.trusted_hosts, plan.session.expose(), egress)
        .await;
    let fallback = match &refreshed {
        Ok(r) => Ok(r.subscribe_url.clone()),
        Err(e) if providers::is_unreachable(e) => match &plan.cached_url {
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
    provider.descriptor(&fallback?)
}

impl AppCore {
    pub(in crate::app) fn provider_egress(&self) -> Egress {
        Egress {
            policy: self.settings.provider_egress,
            core_port: self.core_port(),
        }
    }

    pub(super) fn store_account_identity(
        &self,
        id: &str,
        identity: &SecretString,
    ) -> AppResult<()> {
        let key = identity_key(id);
        let value = identity.expose().as_bytes();
        let current = self.secrets.get(&key)?;
        if current.as_ref().map(|z| z.as_slice()) != Some(value) {
            self.secrets.put(&key, value)?;
        }
        Ok(())
    }

    /// Only expose the saved identifier after an explicit UI request.
    pub(in crate::app) fn reveal_account(&mut self, id: &str, visible: bool) {
        self.view.revealed_account = None;
        if !visible || self.accounts.get(id).is_none() {
            return;
        }
        match self.secrets.get(&identity_key(id)) {
            Ok(identity) => {
                self.view.revealed_account = identity.map(|bytes| {
                    let name = String::from_utf8_lossy(&bytes).into_owned();
                    (id.into(), SecretString::new(name))
                });
            }
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
    }
    /// §7.2: the token in the URL may rotate; keep the newest one.
    pub(super) fn store_subscribe_url(&self, key: &str, url: &SecretString) {
        let current = self.secrets.get(key).ok().flatten();
        let value = url.expose().as_bytes();
        if current.as_ref().map(|z| z.as_slice()) == Some(value) {
            return;
        }
        tracing::info!("subscription url changed, stored the new one");
        if let Err(e) = self.secrets.put(key, url.expose().as_bytes()) {
            tracing::warn!(error = %e, "subscribe url not stored");
        }
    }

    /// Deleting an account-backed profile also clears its local account data.
    pub(in crate::app) fn remove_account(&mut self, id: &str) -> AppResult<()> {
        let Some(account) = self.accounts.get(id).cloned() else {
            return Ok(());
        };
        self.secrets.delete(&identity_key(id))?;
        for key in [&account.session_secret, &account.subscribe_secret]
            .into_iter()
            .flatten()
        {
            self.secrets.delete(key)?;
        }
        let index = self
            .accounts
            .file
            .accounts
            .iter()
            .position(|a| a.id == id)
            .unwrap();
        self.accounts.file.accounts.remove(index);
        if let Err(e) = self.accounts.save(&self.layout) {
            self.accounts.file.accounts.insert(index, account);
            return Err(e);
        }
        self.view.revealed_account = None;
        self.validating_prev.remove(id);
        self.view.login_message = None;
        self.refresh_accounts_view();
        Ok(())
    }

    /// Plan for updating an account-backed profile; errors explain why not.
    pub(in crate::app) fn provider_plan(&self, account_id: &str) -> AppResult<ProviderPlan> {
        let account = self.accounts.get(account_id).ok_or_else(|| {
            AppError::new(
                AppErrorKind::AuthInvalid,
                STAGE,
                "关联的服务商账号已移除，请重新添加账号",
            )
        })?;
        if account.status == AccountStatus::ReauthRequired {
            return Err(AppError::new(
                AppErrorKind::AuthInvalid,
                STAGE,
                format!(
                    "{}登录已失效，请在“配置”页重新添加账号",
                    providers::account_name(&account.provider_id)
                ),
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
            .and_then(|b| String::from_utf8(b.to_vec()).ok())
            .map(SecretString::new);
        Ok(ProviderPlan {
            account_id: account.id.clone(),
            provider_id: account.provider_id.clone(),
            trusted_hosts: account.trusted_hosts.clone(),
            session,
            cached_url,
        })
    }

    pub(super) fn load_session(&self, account: &ProviderAccount) -> Option<SecretString> {
        let key = account.session_secret.as_ref()?;
        match self.secrets.get(key) {
            Ok(Some(bytes)) => String::from_utf8(bytes.to_vec())
                .ok()
                .map(SecretString::new),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "session not readable");
                None
            }
        }
    }

    /// Provider usage wins over subscription headers (§15).
    pub(super) fn sync_account_usage(&mut self, account_id: &str) {
        let Some(account) = self.accounts.get(account_id) else {
            return;
        };
        let Some(profile_id) = account.profile_id.clone() else {
            return;
        };
        let usage = account.snapshot.as_ref().and_then(|s| s.usage());
        if let Some(p) = self.profiles.get_mut(&profile_id)
            && p.usage != usage
        {
            p.usage = usage;
            self.save_profiles();
            self.refresh_profiles_view();
        }
    }

    pub(super) fn save_accounts(&mut self) {
        if let Err(e) = self.accounts.save(&self.layout) {
            self.view.notice = Some(error_notice(&e));
        }
    }

    pub(in crate::app) fn refresh_accounts_view(&mut self) {
        self.view.accounts = self
            .accounts
            .all()
            .iter()
            .map(super::state::AccountView::from_account)
            .collect();
    }
}

/// Whether a profile belongs to a provider account.
pub fn account_of(profile: &Profile) -> Option<&str> {
    match &profile.source {
        SubscriptionSource::ProviderAccount { account_id, .. } => Some(account_id),
        _ => None,
    }
}

fn identity_key(id: &str) -> String {
    format!("account/{id}/identity")
}
