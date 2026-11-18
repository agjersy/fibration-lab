//! The artifact index of a session: what its runs produced (figures with their sidecars,
//! tables, other files), named by run id and repo-relative path.
//!
//! The index is built from what the kernel already records: the session's run records
//! (`runs.jsonl`: script, arguments, tree identity) and each run's artifact list (the
//! kernel's `artifacts.jsonl` for a local run, the copy kept with the session for a remote
//! one). No directory is scanned and no layout is guessed. Slides cite analysis results
//! through it: a figure is placed on a slide by run id and artifact path, and the slide
//! element carries the run's citation.

use super::runs::RunService;
use crate::session::Session;
use analyst_protocol::kernel::{Artifact, ArtifactKind};
use analyst_protocol::ui::ArtifactEntry;
use std::collections::BTreeSet;
use std::path::Path;

/// The identity of the project tree as runs record it: the tree a mirror would hold (code,
/// without data, outputs, and decks). `None` outside a git repository.
pub fn current_tree(repo: &Path) -> Option<String> {
    crate::target::sync::tree_id(repo).ok()
}

fn entry(
    run: &analyst_protocol::session::SessionRun,
    a: &Artifact,
    current: Option<&str>,
) -> ArtifactEntry {
    let tree_hash = run.meta.tree_hash.clone();
    ArtifactEntry {
        run_id: run.meta.run_id.clone(),
        path: a.path.clone(),
        kind: a.kind,
        script: run.meta.script.clone(),
        args: run.meta.args.clone(),
        // Stale: the code the run executed is not the code in the working tree now.
        stale: match (&tree_hash, current) {
            (Some(run_tree), Some(now)) => run_tree != now,
            _ => false,
        },
        tree_hash,
        title: a.sidecar.as_ref().and_then(|s| s.title.clone()),
        size: a.size,
        created_at: a.created_at.clone(),
        target: run.target.clone(),
    }
}

/// The artifacts of the session's runs, newest run first. `run_id` and `kind` narrow the
/// list; `limit` bounds it (default 200).
pub fn list(
    runs: &RunService,
    session: &Session,
    repo: &Path,
    run_id: Option<&str>,
    kind: Option<ArtifactKind>,
    limit: Option<usize>,
) -> Vec<ArtifactEntry> {
    let current = current_tree(repo);
    let mut records = session.read_runs().unwrap_or_default();
    records.reverse();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let max = limit.unwrap_or(200).max(1);
    for run in records {
        // `runs.jsonl` has one line per state change of a run; the newest is first here.
        if !seen.insert(run.meta.run_id.clone()) {
            continue;
        }
        if run_id.is_some_and(|w| w != run.meta.run_id) {
            continue;
        }
        for a in runs.artifacts(&run.meta.run_id) {
            if kind.is_some_and(|k| k != a.kind) {
                continue;
            }
            out.push(entry(&run, &a, current.as_deref()));
            if out.len() >= max {
                return out;
            }
        }
    }
    out
}

/// One artifact of one run, by the path the run reported.
pub fn find(
    runs: &RunService,
    session: &Session,
    repo: &Path,
    run_id: &str,
    path: &str,
) -> Option<ArtifactEntry> {
    let wanted = crate::util::normalize_rel_path(path).ok()?;
    list(runs, session, repo, Some(run_id), None, Some(usize::MAX))
        .into_iter()
        .find(|e| e.path == wanted)
}
