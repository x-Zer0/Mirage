//! Account-backed profile creation, revision refresh and external-client import.
use super::account_of;
use crate::app::pipeline::{RemoteAdded, RemoteFetch};
use crate::app::state::{Notice, RuntimeStatus};
use crate::app::{AppCore, Internal, error_notice, pipeline};
use crate::domain::error::AppResult;
use crate::domain::model::{AccountId, SubscriptionSource, UpdatePolicy};
use crate::domain::redact::SecretString;
use crate::providers::{self, account_provider};

impl AppCore {
    /// Creates the account's profile on first login; later logins update it.
    pub(super) fn ensure_account_profile(&mut self, account_id: &str, url: Option<SecretString>) {
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
        let provider = self
            .accounts
            .get(account_id)
            .map(|a| a.provider_id.clone())
            .unwrap_or_default();
        let desc = match account_provider(&provider).and_then(|p| p.descriptor(&url)) {
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
        self.view.busy_tasks.insert(format!(
            "正在下载并校验{}订阅…",
            providers::account_name(&provider)
        ));
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

    pub(in crate::app) fn on_provider_profile_added(
        &mut self,
        account_id: AccountId,
        result: AppResult<RemoteAdded>,
    ) {
        self.view.busy_tasks.clear();
        let Some(account) = self.accounts.get(&account_id) else {
            return;
        };
        let provider_id = account.provider_id.clone();
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                self.record_operation(
                    "新增订阅",
                    providers::account_name(&provider_id),
                    "手动 · 账号订阅",
                    "失败",
                    &e.user_message(),
                );
                self.view.notice = Some(Notice::error(format!(
                    "{}订阅未能导入：{}",
                    providers::account_name(&provider_id),
                    e.user_message()
                )));
                return;
            }
        };
        let source = SubscriptionSource::ProviderAccount {
            provider_id: provider_id.clone(),
            account_id: account_id.clone(),
        };
        let profile_id = outcome.profile_id.clone();
        if self.store_new_profile(
            outcome,
            providers::account_name(&provider_id).into(),
            source,
        ) {
            if let Some(a) = self.accounts.get_mut(&account_id) {
                a.profile_id = Some(profile_id);
            }
            self.save_accounts();
            self.sync_account_usage(&account_id);
            self.refresh_accounts_view();
        }
    }

    /// Let each adapter decide whether an old account revision needs refreshing.
    /// Success uses the normal checked/atomic commit; failures keep the old revision.
    pub(in crate::app) fn migrate_provider_profiles(&mut self) {
        if self.binary.is_none()
            || matches!(
                self.view.runtime,
                RuntimeStatus::Starting | RuntimeStatus::Restarting | RuntimeStatus::Reloading
            )
        {
            return;
        }
        let ids: Vec<_> = self
            .profiles
            .all()
            .iter()
            .filter(|p| account_of(p).is_some())
            .map(|p| p.id.clone())
            .collect();
        for id in ids {
            let Some(profile) = self.profiles.get(&id) else {
                continue;
            };
            let SubscriptionSource::ProviderAccount { provider_id, .. } = &profile.source else {
                continue;
            };
            let Ok(provider) = account_provider(provider_id) else {
                continue;
            };
            let revision = profile
                .active_revision
                .as_ref()
                .map(|rev| crate::subscriptions::revisions::find(&self.layout, profile, rev))
                .transpose();
            match revision {
                Ok(revision)
                    if !provider
                        .needs_revision_update(revision.as_ref().and_then(|r| r.as_ref())) => {}
                Ok(_) => self.update_profile(&id, true),
                Err(e) => self.view.notice = Some(error_notice(&e)),
            }
        }
    }

    pub(in crate::app) fn import_legacy(&mut self, provider_id: &str) {
        let provider = match account_provider(provider_id) {
            Ok(p) => p,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        let Some(binary) = self.require_binary() else {
            return;
        };
        let layout = self.layout.clone();
        let port = self.view.mixed_port;
        let task_name = format!("正在导入原{}客户端配置…", provider.name());
        self.view.busy_tasks.insert(task_name);
        self.spawn(async move {
            let result = async {
                let raw = provider.import_legacy()?;
                pipeline::import_raw(
                    &binary,
                    &layout,
                    format!("{}导入", provider.name()),
                    raw,
                    port,
                )
                .await
            }
            .await;
            Internal::Imported(format!("{}导入", provider.name()), result)
        });
    }
}
