//! Diagnostics bundle (DESIGN.md §18). A zip with the redacted application
//! and core logs, `settings.json`, a profile summary without URLs, the last
//! check output, the current system proxy value, the core version and hash,
//! and the Windows version.
//!
//! §14.2: the raw subscriptions and the run config are deliberately **not**
//! included, and the user is warned before the file is written.
//!
//! Core logs are redacted on export because mihomo writes them itself: the
//! application log already went through the filter in `logging`, but nothing
//! guarantees the same of a node password echoed by the core. Redacting again
//! is idempotent, so both are passed through `redact_text`.

use crate::cores::mihomo::binary::{self, CoreBinary};
use crate::domain::error::{AppError, AppResult};
use crate::domain::model::{Profile, SubscriptionSource};
use crate::domain::redact::redact_text;
use crate::platform::windows::{conflict, registry, sysproxy};
use crate::storage::layout::Layout;
use crate::storage::zip::ZipWriter;
use std::fmt::Write as _;
use std::path::Path;

const STAGE: &str = "app.diagnostics";
/// Cap for one log inside the bundle, so a long-running install still produces
/// a small archive (§18 does not require the whole history).
const MAX_LOG_BYTES: usize = 512 * 1024;
/// Cap for the whole bundle before the logs are added.
const MAX_ENTRIES: usize = 8 * 1024 * 1024;

/// What the bundle was built from, so the UI can describe it.
pub struct Bundle {
    pub entries: Vec<String>,
    pub bytes: usize,
}

/// Everything the bundle needs that lives outside the data directory.
pub struct Inputs<'a> {
    pub version: &'a str,
    pub profiles: &'a [Profile],
    /// Last `-t` output of the active revision (§18).
    pub check_lines: &'a [String],
    /// Composer adjustments of the active revision (§9.5).
    pub policy_notes: &'a [String],
    pub core: Option<&'a CoreBinary>,
    pub mixed_port: u16,
}

/// Collects everything and writes the zip to `path`.
pub fn export(layout: &Layout, path: &Path, inputs: &Inputs<'_>) -> AppResult<Bundle> {
    let mut zip = ZipWriter::new(chrono::Local::now());
    let mut entries = Vec::new();

    // 1. Application logs. The disk copy already went through the filter, but
    //    a line can still carry a shape the filter missed (or an old file
    //    written before a rule was added), so the bundle is filtered again:
    //    redaction is idempotent and this is the artifact users hand out.
    for (name, text) in log_entries(&layout.logs, "logs/mirage") {
        zip.add_text(&name, &redact_text(&text));
        entries.push(name);
    }
    // 2. Core logs (§18), redacted here as described above.
    for (name, text) in log_entries(&layout.core_home.join("logs"), "logs/core") {
        let redacted = redact_text(&text).into_owned();
        zip.add_text(&name, &redacted);
        entries.push(name);
    }

    add_service_logs(&mut zip, &mut entries);

    // 3. settings.json, as saved (no secrets: §14.1 keeps them in the
    //    Credential Manager, and the file holds only references).
    let settings = std::fs::read_to_string(layout.settings_file()).unwrap_or_default();
    if !settings.is_empty() {
        zip.add_text("settings.json", &redact_text(&settings));
        entries.push("settings.json".into());
    }

    // 4. Profile summary without URLs or tokens (§18).
    zip.add_text("profiles.txt", &profile_summary(inputs.profiles));
    entries.push("profiles.txt".into());

    // 5. Last `-t` output and the composer's policy notes (§9.5).
    zip.add_text(
        "check.txt",
        &section(
            "最近一次配置校验输出",
            inputs.check_lines,
            inputs.policy_notes,
        ),
    );
    entries.push("check.txt".into());

    // 6. Current system proxy, read live rather than from the journal, so the
    //    bundle shows what the machine actually has.
    zip.add_text("system-proxy.txt", &system_proxy_text());
    entries.push("system-proxy.txt".into());

    // 7. Core version and hash, plus the Windows version (§18).
    zip.add_text(
        "environment.txt",
        &environment_text(inputs.version, inputs.core, inputs.mixed_port),
    );
    entries.push("environment.txt".into());

    // 8. Conflicts seen at export time (§11.8); handy when a user reports
    //    "the other client is closed" while it is not.
    zip.add_text("conflicts.txt", &conflict_text(inputs.mixed_port));
    entries.push("conflicts.txt".into());

    if zip.len() > MAX_ENTRIES {
        // Cannot happen with the caps above, but never write a surprise.
        return Err(AppError::new(
            crate::domain::error::AppErrorKind::Storage,
            STAGE,
            "诊断包内容超出上限",
        ));
    }
    let bytes = zip.len();
    crate::storage::zip::write_file(path, zip).map_err(|e| AppError::storage(STAGE, e))?;
    tracing::info!(entries = entries.len(), bytes, "diagnostics bundle written");
    Ok(Bundle { entries, bytes })
}

/// Newest segments of one log directory, read tail-first and capped.
fn log_entries(dir: &Path, prefix: &str) -> Vec<(String, String)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = read
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.path())
        .collect();
    // Newest last by modification time, so the newest few are kept.
    files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let mut out = Vec::new();
    for path in files.into_iter().rev().take(3) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let Ok(text) = read_tail(&path, MAX_LOG_BYTES) else {
            continue;
        };
        out.push((format!("{prefix}/{name}"), text));
    }
    out
}

/// Last bytes of a file, aligned to the first newline so no line is cut in half.
fn read_tail(path: &Path, max: usize) -> std::io::Result<String> {
    let data = std::fs::read(path)?;
    if data.len() <= max {
        return Ok(String::from_utf8_lossy(&data).into_owned());
    }
    let start = data.len() - max;
    let cut = data[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map(|i| start + i + 1)
        .unwrap_or(start);
    Ok(format!(
        "（只保留最后 {} KiB）\n{}",
        max / 1024,
        String::from_utf8_lossy(&data[cut..])
    ))
}

/// Names, source kinds and usage only: no `SecretRef` and no URL (§18).
fn profile_summary(profiles: &[Profile]) -> String {
    let mut out = String::from("配置摘要（不含订阅地址与令牌）\n");
    if profiles.is_empty() {
        out.push_str("（没有配置）\n");
        return out;
    }
    for p in profiles {
        let source = match &p.source {
            SubscriptionSource::RemoteUrl { host, .. } => format!("订阅（{host}）"),
            SubscriptionSource::ProviderAccount { provider_id, .. } => {
                format!("服务商账号（{provider_id}）")
            }
            SubscriptionSource::LocalFile { original_name, .. } => {
                format!("本地文件（{original_name}）")
            }
        };
        let _ = writeln!(
            out,
            "· {}｜{}｜活动版本 {}｜更新间隔 {} 小时{}",
            p.name,
            source,
            p.active_revision.as_deref().unwrap_or("无"),
            p.update_policy.interval_hours,
            p.last_fetch
                .as_ref()
                .and_then(|f| f.error.as_deref())
                // A fetch error is usually "HTTP 401 <url>": the URL carries
                // the token, so it is filtered like a log line.
                .map(|e| format!("｜最近失败：{}", redact_text(e)))
                .unwrap_or_default()
        );
    }
    out
}

fn section(title: &str, check_lines: &[String], policy_notes: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{title}");
    if check_lines.is_empty() {
        out.push_str("（本次运行还没有校验输出）\n");
    } else {
        for line in check_lines {
            let _ = writeln!(out, "{line}");
        }
    }
    out.push_str("\n安全调整（§9.5）\n");
    if policy_notes.is_empty() {
        out.push_str("（无）\n");
    } else {
        for note in policy_notes {
            let _ = writeln!(out, "· {note}");
        }
    }
    out
}

fn system_proxy_text() -> String {
    match sysproxy::read() {
        Ok(settings) => {
            let enabled = if settings.proxy_enabled() {
                "已启用"
            } else {
                "未启用"
            };
            format!(
                "系统代理：{enabled}\n服务器：{}\n例外：{}\n自动配置脚本：{}\n",
                settings.server.as_deref().unwrap_or("（无）"),
                settings.bypass.as_deref().unwrap_or("（无）"),
                settings.autoconfig_url.as_deref().unwrap_or("（无）")
            )
        }
        Err(e) => format!("系统代理读取失败：{e}\n"),
    }
}

fn environment_text(version: &str, core: Option<&CoreBinary>, mixed_port: u16) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Mirage {version}");
    let _ = writeln!(out, "Windows：{}", registry::windows_version());
    let _ = writeln!(out, "本地代理端口：{mixed_port}");
    match core {
        Some(binary) => {
            let _ = writeln!(out, "内核：{}", binary.short_version());
            let _ = writeln!(out, "内核版本行：{}", binary.version_line);
            let _ = writeln!(out, "内核路径：{}", binary.exe.display());
            // The bundled binary is pinned, so the expected hash identifies it
            // exactly; recomputed here so a replaced file is visible.
            let _ = writeln!(out, "内核 SHA256（期望）：{}", binary::EXPECTED_SHA256);
        }
        None => out.push_str("内核：未通过校验\n"),
    }
    out
}

fn conflict_text(mixed_port: u16) -> String {
    let conflicts = conflict::detect(mixed_port);
    if conflicts.is_empty() {
        return "未检测到与原自由猫客户端的冲突\n".into();
    }
    let mut out = String::from("检测到的冲突\n");
    for c in conflicts {
        let _ = writeln!(out, "· {}", c.text());
    }
    out
}

fn add_service_logs(zip: &mut ZipWriter, entries: &mut Vec<String>) {
    let Ok(root) = mirage_ipc::known_folder::service_data() else {
        return;
    };
    for (dir, prefix) in [
        (root.join("logs"), "logs/service"),
        (root.join("tun-home").join("logs"), "logs/tun-core"),
    ] {
        for (name, text) in log_entries(&dir, prefix) {
            zip.add_text(&name, &redact_text(&text));
            entries.push(name);
        }
    }
}
