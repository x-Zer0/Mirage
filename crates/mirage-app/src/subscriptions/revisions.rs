//! Raw revision storage and pruning (DESIGN.md §8 steps 4, 9).

use crate::domain::error::{AppError, AppResult};
use crate::domain::model::{ConfigRevision, Profile, RevisionId};
use crate::storage::layout::Layout;
use crate::storage::{atomic, meta};
use sha2::{Digest, Sha256};

const STAGE: &str = "subscriptions.revisions";
const KEEP_REVISIONS: usize = 5;

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn store_raw(
    layout: &Layout,
    profile: &Profile,
    rev: &RevisionId,
    raw: &[u8],
) -> AppResult<()> {
    let path = layout.raw_revision(&profile.id, rev);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| AppError::storage(STAGE, e))?;
    }
    atomic::write(&path, raw).map_err(|e| AppError::storage(STAGE, e))
}

pub fn read_raw(layout: &Layout, profile: &Profile, rev: &RevisionId) -> AppResult<Vec<u8>> {
    std::fs::read(layout.raw_revision(&profile.id, rev)).map_err(|e| AppError::storage(STAGE, e))
}

pub fn load(layout: &Layout, profile: &Profile) -> AppResult<meta::RevisionsFile> {
    meta::load(&layout.revisions_file(&profile.id), STAGE)
}

/// Inserts or replaces `rev`, keeps the newest revisions plus active and
/// last-good, and deletes raw files of pruned ones.
pub fn upsert(layout: &Layout, profile: &Profile, rev: ConfigRevision) -> AppResult<()> {
    let mut file = load(layout, profile)?;
    file.revisions.retain(|r| r.id != rev.id);
    file.revisions.push(rev);
    file.revisions.sort_by(|a, b| {
        b.fetched_at
            .cmp(&a.fetched_at)
            .then_with(|| b.id.cmp(&a.id))
    });

    let pinned = [&profile.active_revision, &profile.last_good_revision];
    let mut kept = Vec::new();
    for (i, r) in file.revisions.drain(..).enumerate() {
        let is_pinned = pinned.iter().any(|p| p.as_deref() == Some(r.id.as_str()));
        if i < KEEP_REVISIONS || is_pinned {
            kept.push(r);
        } else {
            // Best effort: an orphaned raw file is harmless and retried next prune.
            let _ = std::fs::remove_file(layout.raw_revision(&profile.id, &r.id));
        }
    }
    file.revisions = kept;
    meta::save(&layout.revisions_file(&profile.id), &mut file, STAGE)
}

pub fn find(
    layout: &Layout,
    profile: &Profile,
    id: &RevisionId,
) -> AppResult<Option<ConfigRevision>> {
    Ok(load(layout, profile)?
        .revisions
        .into_iter()
        .find(|r| &r.id == id))
}
