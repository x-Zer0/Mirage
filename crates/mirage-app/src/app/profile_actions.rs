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
            let object = picked
                .as_ref()
                .and_then(|f| f.path().file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let result = match picked {
                Some(file) => pipeline::import_local(&binary, &layout, file.path(), port).await,
                None => Err(AppError::new(AppErrorKind::Cancelled, STAGE, "")),
            };
            let _ = tx.send(Internal::Imported(object, result));
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
            Internal::Imported(
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                pipeline::import_local(&binary, &layout, &path, port).await,
            )
        });
    }

    pub(super) fn on_imported(
        &mut self,
        object: String,
        result: Result<pipeline::ImportOutcome, AppError>,
    ) {
        self.view.busy = None;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) if e.kind == AppErrorKind::Cancelled => return,
            Err(e) => {
                self.record_operation(
                    "导入配置",
                    &object,
                    "手动 · 本地文件",
                    "失败",
                    &e.user_message(),
                );
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
            self.record_operation(
                "导入配置",
                &outcome.name,
                "手动 · 本地文件",
                "失败",
                &e.user_message(),
            );
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.profiles.add(profile);
        if let Err(e) = self.profiles.save(&self.layout) {
            self.record_operation(
                "导入配置",
                &outcome.name,
                "手动 · 本地文件",
                "失败",
                &e.user_message(),
            );
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.record_operation(
            "导入配置",
            &outcome.name,
            "手动 · 本地文件",
            "成功",
            &format!(
                "导入 {} 个节点；{}",
                outcome.revision.summary.proxies,
                outcome.revision.policy_notes.join("；")
            ),
        );
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
        let name = self.profile_log_name(id);
        if let Err(e) = self
            .profiles
            .select(id)
            .and_then(|()| self.profiles.save(&self.layout))
        {
            self.record_operation("切换配置", &name, "手动", "失败", &e.user_message());
            self.view.notice = Some(error_notice(&e));
            return;
        }
        self.record_operation(
            "切换配置",
            &name,
            "手动",
            "成功",
            "已保存选择；运行状态另行记录",
        );
        self.refresh_profiles_view();
        if self.running.is_some() || self.view.runtime == crate::app::state::RuntimeStatus::Starting
        {
            self.generation += 1; // 作废旧启动任务
            if let Some(previous) = self.running.take() {
                // Switching is one commit transaction: the old core must release the
                // port before the new one starts, so both steps run in one task.
                self.view.groups.clear();
                self.connection_source = "切换配置";
                self.start_after(Some(previous));
            } else {
                // Starting 期间没有 running，直接启动新配置
                self.connection_source = "切换配置";
                self.start_after(None);
            }
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
            self.record_operation("删除配置", &profile.name, "手动", "失败", &e.user_message());
            self.view.notice = Some(error_notice(&e));
            return;
        }
        if self.running.as_ref().is_some_and(|r| r.profile_id == id) {
            self.disconnect();
        }
        self.finish_update_log(id, "失败", "配置已删除，更新已取消");
        if self.profiles.remove(id).is_some() {
            if self.save_profiles() {
                self.record_operation(
                    "删除配置",
                    &profile.name,
                    "手动",
                    "成功",
                    "配置已从列表删除",
                );
            } else {
                self.record_operation(
                    "删除配置",
                    &profile.name,
                    "手动",
                    "失败",
                    "配置列表保存失败",
                );
            }
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
