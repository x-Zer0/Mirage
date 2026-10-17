//! Local imports, profile selection/removal and profile list refresh.
use super::{AppCore, Internal, error_notice, info_notice, pipeline, profile_view::profile_view};
use crate::domain::error::{AppError, AppErrorKind};
use crate::domain::model::{Profile, SubscriptionSource, now};
use crate::subscriptions::revisions;

const STAGE: &str = "app.handlers";

impl AppCore {
    pub(super) fn pick_and_import(&mut self) {
        let tx = self.internal_tx.clone();
        let layout = self.layout.clone();
        let Some(binary) = self.require_binary() else {
            return;
        };
        let port = self.view.mixed_port;
        self.view.busy = Some("正在导入配置…".into());
        tokio::spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("导入 Mihomo 配置")
                .add_filter("YAML 配置", &["yaml", "yml"])
                .add_filter("所有文件", &["*"])
                .pick_file()
                .await;
            let result = match picked {
                Some(file) => pipeline::import_local(&binary, &layout, file.path(), port).await,
                None => Err(AppError::new(AppErrorKind::Cancelled, STAGE, "")),
            };
            let _ = tx.send(Internal::Imported(result));
        });
    }

    pub(super) fn import(&mut self, path: std::path::PathBuf) {
        let Some(binary) = self.require_binary() else {
            return;
        };
        let layout = self.layout.clone();
        let port = self.view.mixed_port;
        self.view.busy = Some("正在导入配置…".into());
        self.spawn(async move {
            Internal::Imported(pipeline::import_local(&binary, &layout, &path, port).await)
        });
    }

    pub(super) fn on_imported(&mut self, result: Result<pipeline::ImportOutcome, AppError>) {
        self.view.busy = None;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) if e.kind == AppErrorKind::Cancelled => return,
            Err(e) => {
                self.view.notice = Some(error_notice(&e));
                return;
            }
        };
        let profile = Profile {
            id: outcome.profile_id.clone(),
            name: outcome.name.clone(),
            source: SubscriptionSource::LocalFile {
                original_name: outcome.name.clone(),
                imported_at: now(),
            },
            active_revision: Some(outcome.revision.id.clone()),
            last_good_revision: Some(outcome.revision.id.clone()),
            updated_at: now(),
            // Local files are never fetched; the interval only matters for URLs.
            update_policy: Default::default(),
            usage: None,
            last_fetch: None,
        };
        let stored =
            revisions::store_raw(&self.layout, &profile, &outcome.revision.id, &outcome.raw)
                .and_then(|()| revisions::upsert(&self.layout, &profile, outcome.revision.clone()));
        if let Err(e) = stored {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.profiles.add(profile);
        if let Err(e) = self.profiles.save(&self.layout) {
            self.view.notice = Some(error_notice(&e));
        }
        self.view.policy_notes = outcome.revision.policy_notes.clone();
        self.view.check_lines = outcome.check_lines;
        let s = outcome.revision.summary;
        self.view.notice = Some(info_notice(format!(
            "已导入“{}”：{} 个节点，{} 个组，{} 条规则",
            outcome.name, s.proxies, s.groups, s.rules
        )));
        self.refresh_profiles_view();
    }

    pub(super) fn select_profile(&mut self, id: &str) {
        if self.profiles.selected_id().map(String::as_str) == Some(id) {
            return;
        }
        if let Err(e) = self
            .profiles
            .select(id)
            .and_then(|()| self.profiles.save(&self.layout))
        {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.refresh_profiles_view();
        if let Some(previous) = self.running.take() {
            // Switching is one commit transaction: the old core must release the
            // port before the new one starts, so both steps run in one task.
            self.view.groups.clear();
            self.start_after(Some(previous));
        }
    }

    pub(super) fn remove_profile(&mut self, id: &str) {
        if self.view.login_busy {
            return;
        }
        let Some(profile) = self.profiles.get(id).cloned() else {
            return;
        };
        if let Err(e) = self.forget_secrets(&profile) {
            self.view.notice = Some(error_notice(&e));
            return;
        }
        if self.running.as_ref().is_some_and(|r| r.profile_id == id) {
            self.disconnect();
        }
        if self.profiles.remove(id).is_some() {
            self.save_profiles();
            if self
                .queued_reload
                .as_ref()
                .is_some_and(|(queued, _)| queued == id)
            {
                self.queued_reload = None;
            }
            let dir = self.layout.profile_dir(&id.to_owned());
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(error = %e, "profile directory not removed");
            }
        }
        self.refresh_profiles_view();
    }

    pub(super) fn refresh_profiles_view(&mut self) {
        let selected = self.profiles.selected_id().cloned().unwrap_or_default();
        self.view.selected_profile = selected.clone();
        self.view.profiles = self
            .profiles
            .all()
            .iter()
            .map(|p| profile_view(&self.layout, p, &selected, self.updating.contains(&p.id)))
            .collect();
    }
}
