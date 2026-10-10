//! File-change journal.
//!
//! Every write an agent makes is recorded with a snapshot of the previous
//! contents, so any single change — or a whole chat's worth — can be undone
//! later. This is what makes it safe to let a model edit files unattended: the
//! undo path exists before the edit does.

use crate::db::{Db, FileChange};
use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};

/// FNV-1a. Only used to notice that a file changed underneath us, so speed and
/// zero dependencies matter far more than cryptographic strength.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{h:016x}")
}

fn snapshot_dir(chat_id: &str) -> PathBuf {
    crate::db::data_dir().join("snapshots").join(chat_id)
}

/// Who made a change and on whose behalf.
pub struct Provenance<'a> {
    pub chat_id: &'a str,
    pub run_id: Option<&'a str>,
    pub tool_call_id: Option<&'a str>,
    /// `agent` for a tool write, `manual` for an edit made in the app.
    pub origin: &'a str,
}

impl<'a> Provenance<'a> {
    pub fn agent(chat_id: &'a str, run_id: &'a str, tool_call_id: &'a str) -> Self {
        Self { chat_id, run_id: Some(run_id), tool_call_id: Some(tool_call_id), origin: "agent" }
    }

    pub fn manual(chat_id: &'a str) -> Self {
        Self { chat_id, run_id: None, tool_call_id: None, origin: "manual" }
    }
}

/// Records a write and stashes the prior contents.
///
/// Snapshot failures are not fatal — losing undo for one file is much better
/// than failing the edit the user asked for — but the change is still recorded
/// so it shows up in the chat's change list.
pub async fn record(
    db: &Db,
    who: Provenance<'_>,
    path: &Path,
    before: Option<&[u8]>,
    after: &[u8],
) -> Result<String> {
    let Provenance { chat_id, run_id, tool_call_id, origin } = who;
    let id = crate::db::new_id();

    let before_path = match before {
        Some(bytes) => {
            let dir = snapshot_dir(chat_id);
            let dest = dir.join(&id);
            match tokio::fs::create_dir_all(&dir).await {
                Ok(()) => tokio::fs::write(&dest, bytes).await.ok().map(|_| dest.display().to_string()),
                Err(_) => None,
            }
        }
        None => None,
    };

    db.record_file_change(&FileChange {
        id: id.clone(),
        chat_id: chat_id.to_string(),
        run_id: run_id.map(str::to_owned),
        tool_call_id: tool_call_id.map(str::to_owned),
        path: path.display().to_string(),
        op: if before.is_some() { "modify".into() } else { "create".into() },
        origin: origin.to_string(),
        before_path,
        after_hash: Some(content_hash(after)),
        bytes_before: before.map(|b| b.len() as i64).unwrap_or(0),
        bytes_after: after.len() as i64,
        reverted: false,
        created_at: chrono::Utc::now().timestamp_millis(),
    })?;
    Ok(id)
}

/// Records a deletion, stashing the contents so revert can restore them.
/// The generic revert path already handles it: any op that is not "create"
/// restores the snapshot.
pub async fn record_delete(db: &Db, who: Provenance<'_>, path: &Path, before: &[u8]) -> Result<String> {
    let Provenance { chat_id, run_id, tool_call_id, origin } = who;
    let id = crate::db::new_id();

    let dir = snapshot_dir(chat_id);
    let dest = dir.join(&id);
    let before_path = match tokio::fs::create_dir_all(&dir).await {
        Ok(()) => tokio::fs::write(&dest, before).await.ok().map(|_| dest.display().to_string()),
        Err(_) => None,
    };

    db.record_file_change(&FileChange {
        id: id.clone(),
        chat_id: chat_id.to_string(),
        run_id: run_id.map(str::to_owned),
        tool_call_id: tool_call_id.map(str::to_owned),
        path: path.display().to_string(),
        op: "delete".into(),
        origin: origin.to_string(),
        before_path,
        after_hash: None,
        bytes_before: before.len() as i64,
        bytes_after: 0,
        reverted: false,
        created_at: chrono::Utc::now().timestamp_millis(),
    })?;
    Ok(id)
}

pub struct RevertOutcome {
    pub path: String,
    pub restored_bytes: i64,
    /// True when the file had been edited since the change was made, so the
    /// revert discarded work that the journal did not know about.
    pub was_modified_since: bool,
}

/// Undoes one recorded change. Creating a file is undone by deleting it;
/// modifying one is undone by restoring the snapshot.
pub async fn revert(db: &Db, change_id: &str, force: bool) -> Result<RevertOutcome> {
    let change = db.get_file_change(change_id)?;
    if change.reverted {
        return Err(AppError::msg("this change has already been reverted"));
    }
    let path = PathBuf::from(&change.path);

    let current = tokio::fs::read(&path).await.ok();
    let drifted = match (&current, &change.after_hash) {
        (Some(bytes), Some(expected)) => &content_hash(bytes) != expected,
        (None, Some(_)) => true,
        _ => false,
    };
    if drifted && !force {
        return Err(AppError::msg(format!(
            "{} has changed since this edit. Reverting would discard that work — confirm to proceed.",
            change.path
        )));
    }

    let restored = match change.op.as_str() {
        "create" => {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| AppError::msg(format!("could not delete {}: {e}", change.path)))?;
            0
        }
        _ => {
            let snapshot = change
                .before_path
                .as_deref()
                .ok_or_else(|| AppError::msg("no snapshot was stored for this change"))?;
            let bytes = tokio::fs::read(snapshot)
                .await
                .map_err(|e| AppError::msg(format!("snapshot is missing: {e}")))?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&path, &bytes).await?;
            bytes.len() as i64
        }
    };

    db.mark_reverted(change_id)?;
    Ok(RevertOutcome { path: change.path, restored_bytes: restored, was_modified_since: drifted })
}

/// Contents captured before a change, for the diff viewer.
pub async fn before_contents(change: &FileChange) -> Option<String> {
    match &change.before_path {
        Some(p) => tokio::fs::read_to_string(p).await.ok(),
        None => Some(String::new()),
    }
}
