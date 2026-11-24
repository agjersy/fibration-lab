//! The undo timeline of a session: the git checkpoints of its repository
//! (`refs/fibration/checkpoints/*`), which cover code and decks together. A checkpoint is taken
//! before every agent turn that can change files, before a deck is switched to code mode,
//! before a template is replaced, and before an export; restoring one puts the working tree
//! (scripts and slide documents alike) back, after taking a checkpoint of the state it
//! replaces, so a restore can itself be undone. This replaces the deck workbench's own
//! snapshot store.

use super::error::{ServiceError, ServiceResult};
use crate::checkpoint;
use crate::session::Session;
use fibration_protocol::transcript::Event;
use fibration_protocol::ui::CheckpointView;
use std::collections::HashMap;
use std::path::Path;

/// The id of a checkpoint ref (`refs/fibration/checkpoints/<id>` -> `<id>`).
fn id_of(reference: &str) -> &str {
    reference
        .strip_prefix(checkpoint::REF_PREFIX)
        .unwrap_or(reference)
}

/// checkpoint id -> (conversation, turn id) for the turn checkpoints of every conversation.
/// A turn's checkpoint is recorded right before its user message.
fn turns(session: &Session) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    for conv in session.conversations().unwrap_or_default() {
        let mut turn = 0u32;
        let mut pending: Option<String> = None;
        for e in session.read_transcript(&conv).unwrap_or_default() {
            match e {
                Event::Checkpoint { r#ref, .. } => pending = Some(id_of(&r#ref).to_string()),
                Event::User { .. } => {
                    turn += 1;
                    if let Some(id) = pending.take() {
                        out.insert(id, (conv.clone(), format!("{conv}-{turn}")));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Every checkpoint of the repository, newest first, with the turn that took it (if a turn
/// of this session did) or the reason it was taken.
pub fn list(
    repo: &Path,
    session: &Session,
    limit: Option<usize>,
) -> ServiceResult<Vec<CheckpointView>> {
    let turns = turns(session);
    let mut all: Vec<CheckpointView> = checkpoint::history(repo)
        .map_err(|e| ServiceError::internal(format!("{e:#}")))?
        .into_iter()
        .map(|c| {
            let turn = turns.get(&c.id);
            CheckpointView {
                created_at: chrono::DateTime::from_timestamp(c.created as i64, 0)
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_default(),
                label: c.label,
                conversation: turn.map(|t| t.0.clone()),
                turn: turn.map(|t| t.1.clone()),
                id: c.id,
            }
        })
        .collect();
    all.reverse();
    if let Some(n) = limit {
        all.truncate(n.max(1));
    }
    Ok(all)
}

/// The checkpoint taken before a turn (`<conversation>-<n>`).
pub fn turn_checkpoint(session: &Session, turn: &str) -> Option<String> {
    turns(session)
        .into_iter()
        .find(|(_, (_, t))| t == turn)
        .map(|(id, _)| id)
}

/// Restore the working tree to a checkpoint: code and decks. The state it replaces is
/// checkpointed first (label `before restoring <id>`); returns that checkpoint's id.
pub fn restore(repo: &Path, id: &str) -> ServiceResult<String> {
    checkpoint::tree_of(repo, id)
        .map_err(|_| ServiceError::not_found(format!("no checkpoint `{id}`")))?;
    let safety = checkpoint::new_id("restore");
    checkpoint::create_labeled(repo, &safety, &format!("before restoring {id}"))
        .map_err(|e| ServiceError::internal(format!("{e:#}")))?;
    checkpoint::restore(repo, id).map_err(|e| ServiceError::internal(format!("{e:#}")))?;
    Ok(safety)
}

/// Undo a turn: restore the checkpoint taken before it.
pub fn revert_turn(repo: &Path, session: &Session, turn: &str) -> ServiceResult<String> {
    let id = turn_checkpoint(session, turn).ok_or_else(|| {
        ServiceError::not_found(format!(
            "no checkpoint was taken for turn `{turn}` (turns in plan and run mode change nothing)"
        ))
    })?;
    restore(repo, &id)?;
    Ok(id)
}
