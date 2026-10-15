//! Configuration pipeline steps shared by import and connect (DESIGN.md §8).

use crate::cores::mihomo::binary::CoreBinary;
use crate::cores::mihomo::check::{self, CheckReport};
use crate::cores::mihomo::compose::{self, Composed, ManagedOverlay, OVERLAY_VERSION};
use crate::domain::error::AppResult;
use crate::domain::model::{ConfigFormat, ConfigRevision, Validity, new_id, now};
use crate::storage::layout::Layout;
use crate::subscriptions::{detect, import, revisions};
use std::path::Path;

pub struct Prepared {
    pub format: ConfigFormat,
    pub composed: Composed,
    pub report: CheckReport,
}

/// detect → compose → `mihomo -t`. Nothing is written outside `check/`.
pub async fn prepare(
    binary: &CoreBinary,
    layout: &Layout,
    check_id: &str,
    raw: &[u8],
    overlay: &ManagedOverlay<'_>,
) -> AppResult<Prepared> {
    let detected = detect::detect(raw)?;
    let composed = compose::compose(&detected.text, overlay)?;
    let report = check::check(binary, layout, &check_id.to_owned(), &composed.yaml).await?;
    Ok(Prepared {
        format: detected.format,
        composed,
        report,
    })
}

pub struct ImportOutcome {
    pub profile_id: String,
    pub name: String,
    pub raw: Vec<u8>,
    pub revision: ConfigRevision,
    pub check_lines: Vec<String>,
}

/// Validates a local file before any profile is created; an invalid file
/// never becomes a profile.
pub async fn import_local(
    binary: &CoreBinary,
    layout: &Layout,
    path: &Path,
    mixed_port: u16,
) -> AppResult<ImportOutcome> {
    let imported = import::read_local(path)?;
    let profile_id = new_id();
    let rev_id = new_id();
    let secret = super::runtime::random_secret()?;
    let overlay = ManagedOverlay {
        mixed_port,
        controller: super::runtime::PLACEHOLDER_CONTROLLER,
        secret: &secret,
        log_level: super::runtime::CORE_LOG_LEVEL,
        profile_id: &profile_id,
    };
    let prepared = prepare(binary, layout, &rev_id, &imported.raw, &overlay).await?;
    let revision = ConfigRevision {
        id: rev_id,
        raw_sha256: revisions::sha256_hex(&imported.raw),
        format: prepared.format,
        fetched_at: now(),
        checked_with: Some(binary.short_version()),
        overlay_version: OVERLAY_VERSION,
        validity: Validity::Valid,
        summary: prepared.composed.summary,
        policy_notes: prepared.composed.policy_notes,
    };
    Ok(ImportOutcome {
        profile_id,
        name: imported.name,
        raw: imported.raw,
        revision,
        check_lines: prepared.report.lines,
    })
}
