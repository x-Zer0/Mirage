//! Login and replacement of a provider's saved account.
use crate::app::state::Notice;
use crate::app::{AppCore, Internal, error_notice, info_notice};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{AccountId, AccountStatus, ProviderAccount, new_id, now};
use crate::domain::redact::Password;
use crate::providers::{self, LoggedIn, account_provider};

impl AppCore {
    pub(in crate::app) fn provider_login(
        &mut self,
        provider_id: String,
        email: String,
        password: Password,
    ) {
        if self.view.login_busy {
            return;
        }
        let provider = match account_provider(&provider_id) {
            Ok(p) => p,
            Err(e) => {
                self.view.login_message = Some(e.user_message());
                return;
            }
        };
        if self.accounts.by_provider(&provider_id).is_some_and(|a| {
            a.status == AccountStatus::Validating
                || a.profile_id
                    .as_ref()
                    .is_some_and(|id| self.updating.contains(id))
        }) {
            self.view.login_message = Some("正在刷新该账号或订阅，请稍后再登录".into());
            return;
        }
        self.view.login_busy = true;
        self.view.login_message = None;
        let layout = self.layout.clone();
        let egress = self.provider_egress();
        self.spawn(async move {
            Internal::LoggedIn {
                provider_id,
                result: Box::new(provider.login(&layout, email, password, egress).await),
            }
        });
    }

    pub(in crate::app) fn on_logged_in(
        &mut self,
        provider_id: String,
        result: AppResult<LoggedIn>,
    ) {
        self.view.login_busy = false;
        let logged_in = match result {
            Ok(l) => l,
            Err(e) => return self.on_login_failed(&provider_id, e),
        };
        let id = match self.store_account_login(&provider_id, &logged_in) {
            Ok(id) => id,
            Err(e) => {
                self.view.login_message = Some(e.user_message());
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        self.view.login_success += 1;
        tracing::info!(account = %id, "provider account logged in");
        if let Some(e) = logged_in.follow_up_error {
            self.view.notice = Some(Notice::error(format!(
                "已登录，但读取账号信息失败：{}",
                e.user_message()
            )));
        } else {
            self.view.notice = Some(info_notice(format!(
                "已登录{}账号 {}",
                providers::account_name(&provider_id),
                logged_in.email_masked
            )));
        }
        self.sync_account_usage(&id);
        self.ensure_account_profile(&id, logged_in.subscribe_url);
        self.refresh_accounts_view();
    }

    fn store_account_login(
        &mut self,
        provider_id: &str,
        logged_in: &LoggedIn,
    ) -> AppResult<AccountId> {
        // Re-login of the existing account keeps its id (and profile).
        let id = self
            .accounts
            .by_provider(provider_id)
            .map(|a| a.id.clone())
            .unwrap_or_else(new_id);
        let session_key = format!("account/{id}/session");
        self.secrets
            .put(&session_key, logged_in.session.expose().as_bytes())?;
        let url_key = format!("account/{id}/subscribe_url");
        // Clear the old URL before storing the new account's subscription.
        self.secrets.delete(&url_key)?;
        if let Some(url) = &logged_in.subscribe_url {
            self.secrets.put(&url_key, url.expose().as_bytes())?;
        }
        self.store_account_identity(&id, &logged_in.identity)?;
        self.view.revealed_account = None;
        let existing = self.accounts.get(&id).cloned();
        let account = ProviderAccount {
            id: id.clone(),
            provider_id: provider_id.into(),
            display_name: logged_in.email_masked.clone(),
            session_secret: Some(session_key),
            subscribe_secret: Some(url_key),
            // A new login replaces the account, so no old session data is reused.
            trusted_hosts: vec![logged_in.trusted_host.clone()],
            status: AccountStatus::Valid,
            snapshot: logged_in.snapshot.clone(),
            profile_id: existing.as_ref().and_then(|a| a.profile_id.clone()),
            created_at: existing.as_ref().map_or_else(now, |a| a.created_at),
        };
        match self.accounts.get_mut(&id) {
            Some(slot) => *slot = account,
            None => self.accounts.file.accounts.push(account),
        }
        self.accounts.save(&self.layout)?;
        Ok(id)
    }

    fn on_login_failed(&mut self, provider_id: &str, e: AppError) {
        tracing::warn!(error = %e, "provider login failed");
        let text = e.user_message();
        if e.kind == AppErrorKind::NeedsUserAction
            && let Some(id) = self.accounts.by_provider(provider_id).map(|a| a.id.clone())
            && let Some(a) = self.accounts.get_mut(&id)
        {
            a.status = AccountStatus::NeedsUserAction(text.clone());
            self.save_accounts();
        }
        // An existing session survives a failed re-login (§6.4).
        self.view.login_message = Some(text);
        self.refresh_accounts_view();
    }
}
