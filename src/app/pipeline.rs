//! Configuration pipeline steps shared by import and connect (DESIGN.md §8).

use crate::cores::mihomo::binary::CoreBinary;
use crate::cores::mihomo::check::{self, CheckReport};
use crate::cores::mihomo::compose::{self, Composed, ManagedOverlay, OVERLAY_VERSION};
use crate::domain::error::AppResult;
use crate::domain::model::{
    ConfigFormat, ConfigRevision, EgressPolicy, Profile, ProfileId, RevisionId, Validity, new_id,
    now,
};
use crate::storage::layout::Layout;
use crate::subscriptions::fetch::{self, Descriptor};
use crate::subscriptions::userinfo::SubscriptionMeta;
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
    import_raw(binary, layout, imported.name, imported.raw, mixed_port).await
}

/// Same as [`import_local`] for bytes already in memory (legacy import).
pub async fn import_raw(
    binary: &CoreBinary,
    layout: &Layout,
    name: String,
    raw: Vec<u8>,
    mixed_port: u16,
) -> AppResult<ImportOutcome> {
    let profile_id = new_id();
    let rev_id = new_id();
    let (revision, check_lines) =
        check_candidate(binary, layout, &profile_id, &rev_id, &raw, mixed_port).await?;
    Ok(ImportOutcome {
        profile_id,
        name,
        raw,
        revision,
        check_lines,
    })
}

/// prepare() with a throw-away controller, producing a Valid revision record.
async fn check_candidate(
    binary: &CoreBinary,
    layout: &Layout,
    profile_id: &ProfileId,
    rev_id: &RevisionId,
    raw: &[u8],
    mixed_port: u16,
) -> AppResult<(ConfigRevision, Vec<String>)> {
    let secret = super::runtime::random_secret()?;
    let overlay = ManagedOverlay {
        mixed_port,
        controller: super::runtime::PLACEHOLDER_CONTROLLER,
        secret: &secret,
        log_level: super::runtime::CORE_LOG_LEVEL,
        profile_id,
        mode: None,
    };
    let prepared = prepare(binary, layout, rev_id, raw, &overlay).await?;
    let revision = ConfigRevision {
        id: rev_id.clone(),
        raw_sha256: revisions::sha256_hex(raw),
        format: prepared.format,
        fetched_at: now(),
        checked_with: Some(binary.short_version()),
        overlay_version: OVERLAY_VERSION,
        validity: Validity::Valid,
        summary: prepared.composed.summary,
        policy_notes: prepared.composed.policy_notes,
    };
    Ok((revision, prepared.report.lines))
}

/// Inputs shared by adding and updating a URL subscription.
#[derive(Clone)]
pub struct RemoteFetch {
    pub binary: CoreBinary,
    pub layout: Layout,
    pub desc: Descriptor,
    pub egress: EgressPolicy,
    /// Mixed port of a running core, for the Auto/ViaLocalCore routes.
    pub core_port: Option<u16>,
    pub mixed_port: u16,
}

pub struct RemoteAdded {
    pub profile_id: ProfileId,
    pub raw: Vec<u8>,
    pub revision: ConfigRevision,
    pub meta: SubscriptionMeta,
    pub check_lines: Vec<String>,
}

/// fetch → detect → compose → check for a new subscription. Nothing is
/// stored unless every step passed, so a broken URL never becomes a profile.
pub async fn add_remote(req: RemoteFetch) -> AppResult<RemoteAdded> {
    let downloaded = fetch::download(&req.desc, req.egress, req.core_port).await?;
    let profile_id = new_id();
    let rev_id = new_id();
    let (revision, check_lines) = check_candidate(
        &req.binary,
        &req.layout,
        &profile_id,
        &rev_id,
        &downloaded.raw,
        req.mixed_port,
    )
    .await?;
    Ok(RemoteAdded {
        profile_id,
        raw: downloaded.raw,
        revision,
        meta: downloaded.meta,
        check_lines,
    })
}

pub enum UpdateResult {
    /// Same bytes as the active revision; only metadata changed.
    Unchanged,
    /// A new revision passed `-t` and is stored as Valid (not yet active).
    Candidate {
        revision: ConfigRevision,
        check_lines: Vec<String>,
    },
}

pub struct UpdateOutcome {
    pub profile_id: ProfileId,
    pub meta: Option<SubscriptionMeta>,
    pub result: AppResult<UpdateResult>,
}

/// Update of an existing profile (§16.4). The raw candidate is stored first
/// as Unchecked; a failed check marks it Invalid. The active revision is never
/// touched here — committing is the actor's job.
pub async fn update_remote(req: RemoteFetch, profile: Profile) -> UpdateOutcome {
    let downloaded = match fetch::download(&req.desc, req.egress, req.core_port).await {
        Ok(d) => d,
        Err(e) => {
            return UpdateOutcome {
                profile_id: profile.id,
                meta: None,
                result: Err(e),
            };
        }
    };
    let meta = Some(downloaded.meta.clone());
    let result = store_and_check(&req, &profile, &downloaded.raw).await;
    UpdateOutcome {
        profile_id: profile.id,
        meta,
        result,
    }
}

async fn store_and_check(
    req: &RemoteFetch,
    profile: &Profile,
    raw: &[u8],
) -> AppResult<UpdateResult> {
    let sha = revisions::sha256_hex(raw);
    let active_sha = match &profile.active_revision {
        Some(rev) => revisions::find(&req.layout, profile, rev)?.map(|r| r.raw_sha256),
        None => None,
    };
    if active_sha.as_deref() == Some(sha.as_str()) {
        return Ok(UpdateResult::Unchanged);
    }
    // Content detection before anything is stored: HTML pages, empty bodies
    // and link lists are rejected without leaving a revision behind.
    let format = detect::detect(raw)?.format;
    let rev_id = new_id();
    let mut revision = ConfigRevision {
        id: rev_id.clone(),
        raw_sha256: sha,
        format,
        fetched_at: now(),
        checked_with: None,
        overlay_version: OVERLAY_VERSION,
        validity: Validity::Unchecked,
        summary: Default::default(),
        policy_notes: Vec::new(),
    };
    revisions::store_raw(&req.layout, profile, &rev_id, raw)?;
    revisions::upsert(&req.layout, profile, revision.clone())?;
    match check_candidate(
        &req.binary,
        &req.layout,
        &profile.id,
        &rev_id,
        raw,
        req.mixed_port,
    )
    .await
    {
        Ok((checked, check_lines)) => {
            revisions::upsert(&req.layout, profile, checked.clone())?;
            Ok(UpdateResult::Candidate {
                revision: checked,
                check_lines,
            })
        }
        Err(e) => {
            revision.validity = Validity::Invalid(e.user_message());
            revision.checked_with = Some(req.binary.short_version());
            revisions::upsert(&req.layout, profile, revision)?;
            Err(e)
        }
    }
}
