//! User consent, save dialog and completion notice for diagnostics exports.
use super::{AppCore, Internal, diagnostics, error_notice, info_notice};
use crate::domain::error::{AppError, AppResult};

const STAGE: &str = "app.settings";
const VERSION: &str = env!("CARGO_PKG_VERSION");

impl AppCore {
    /// Bundle export (§18): the user picks the file, then everything is
    /// collected on a blocking thread (the log files may be large).
    pub(super) fn export_diagnostics(&mut self) {
        if self.view.exporting {
            return;
        }
        self.view.exporting = true;
        let layout = self.layout.clone();
        let version = VERSION.to_owned();
        let profiles = self.profiles.all().to_vec();
        let check_lines = self.view.check_lines.clone();
        let policy_notes = self
            .view
            .profiles
            .iter()
            .find(|p| p.selected)
            .map(|p| p.policy_notes.clone())
            .unwrap_or_default();
        let core = self.binary.clone();
        let port = self.view.mixed_port;
        self.spawn(async move {
            // §14.2: the user is told what the bundle may contain before it
            // is written, and the default name carries the date.
            let confirmed = rfd::AsyncMessageDialog::new()
                .set_title("导出诊断包")
                .set_description(
                    "诊断包包含脱敏后的应用与内核日志、settings.json、配置摘要\
                     （不含订阅地址）、最近一次校验输出、系统代理与内核信息；已安装时也包含 TUN 服务日志。\n\
                     原始订阅和运行配置不会包含在内。确定继续？",
                )
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show()
                .await;
            if !matches!(confirmed, rfd::MessageDialogResult::Ok) {
                return Internal::DiagnosticsExported(Ok(None));
            }
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("保存诊断包")
                .set_file_name(format!(
                    "mirage-diagnostics-{}.zip",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ))
                .add_filter("ZIP", &["zip"])
                .save_file()
                .await
            else {
                return Internal::DiagnosticsExported(Ok(None));
            };
            let path = file.path().to_path_buf();
            // The blocking pool: zipping log tails is file I/O.
            let built = tokio::task::spawn_blocking(move || {
                diagnostics::export(
                    &layout,
                    &path,
                    &diagnostics::Inputs {
                        version: &version,
                        profiles: &profiles,
                        check_lines: &check_lines,
                        policy_notes: &policy_notes,
                        core: core.as_ref(),
                        mixed_port: port,
                    },
                )
            })
            .await;
            let result = match built {
                Ok(r) => r,
                Err(e) => Err(AppError::internal(STAGE, e)),
            };
            Internal::DiagnosticsExported(result.map(Some))
        });
    }

    pub(super) fn on_diagnostics_exported(
        &mut self,
        result: AppResult<Option<diagnostics::Bundle>>,
    ) {
        self.view.exporting = false;
        match result {
            // Cancelled in the dialog: no toast, nothing happened.
            Ok(None) => {}
            Ok(Some(bundle)) => {
                self.view.notice = Some(info_notice(format!(
                    "诊断包已导出：{} 个文件，{:.1} KiB",
                    bundle.entries.len(),
                    bundle.bytes as f64 / 1024.0
                )));
            }
            Err(e) => self.view.notice = Some(error_notice(&e)),
        }
    }
}
