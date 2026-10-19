//! Background account refresh and startup session validation.
use crate::app::state::{Notice, Retry};
use crate::app::{AppCore, Internal, error_notice, info_notice};
use crate::domain::error::{AppErrorKind, AppResult};
use crate::domain::model::{AccountId, AccountStatus};
use crate::providers::{self, Refreshed, account_provider};

impl AppCore {
    pub(in crate::app) fn revalidate_account(&mut self, id: &str, manual: bool) {
        if self.view.login_busy {
            return;
        }
        let Some(account) = self.accounts.get(id).cloned() else {
            return;
        };
        if account.status == AccountStatus::Validating {
            return;
        }
        let session = match self.load_session(&account) {
            Ok(s) => s,
            Err(e) => {
                if manual {
                    // L6: distinguish storage failure from missing session
                    if e.kind == AppErrorKind::Storage {
                        self.view.notice = Some(error_notice(&e));
                    } else {
                        self.view.notice =
                            Some(info_notice("该账号没有已保存的会话，请重新登录"));
                    }
                }
                return;
            }
        };
        let provider = match account_provider(&account.provider_id) {
            Ok(p) => p,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
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
            let result = provider
                .refresh(&layout, &account.trusted_hosts, session.expose(), egress)
                .await;
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
    pub(in crate::app) fn on_account_refreshed(
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
        let mut identity = None;
        match result {
            Ok(r) => {
                account.status = AccountStatus::Valid;
                account.snapshot = Some(r.snapshot);
                identity = r.identity;
                new_url = Some(r.subscribe_url);
                if manual {
                    self.view.notice = Some(info_notice("账号信息已刷新"));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, account = %id, "account check failed");
                account.status = match e.kind {
                    AppErrorKind::AuthInvalid => AccountStatus::ReauthRequired,
                    _ if providers::is_unreachable(&e) => {
                        AccountStatus::NetworkUnavailable(e.user_message())
                    }
                    _ => previous
                        .filter(|p| *p != AccountStatus::Validating)
                        .unwrap_or(AccountStatus::Valid),
                };
                if manual || e.kind == AppErrorKind::AuthInvalid {
                    let notice = Notice::error(e.user_message()).with_stage(&format!(
                        "{}账号",
                        providers::account_name(&account.provider_id)
                    ));
                    self.view.notice = Some(if e.kind == AppErrorKind::AuthInvalid {
                        notice.notifying()
                    } else {
                        notice.with_retry(Retry::RevalidateAccount(id.clone()))
                    });
                }
            }
        }
        let url_key = account.subscribe_secret.clone();
        if let Some(identity) = identity
            && let Err(e) = self.store_account_identity(&id, &identity)
        {
            self.view.notice = Some(error_notice(&e));
        }
        self.save_accounts();
        if let (Some(url), Some(key)) = (new_url, url_key) {
            self.store_subscribe_url(&key, &url);
        }
        self.sync_account_usage(&id);
        self.refresh_accounts_view();
    }

    /// Background session check of every saved account (§16.1, §6.4).
    pub(in crate::app) fn validate_accounts_on_start(&mut self) {
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
}
