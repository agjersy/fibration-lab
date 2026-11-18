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

/// An element placed on a slide from the analysis (a figure of a run, an equation of a
/// function): the slide document with the element and its citation (not written yet), and the
/// hash of the slide file it was made from.
pub struct PlacedElement {
    pub slide: serde_json::Value,
    pub hash: String,
}

/// Place a figure of an analysis run on a slide: look the figure up in the artifact index, copy
/// the image into the deck's `assets/` (content-addressed, so the deck stays self-contained;
/// data files are never copied), and fill a placeholder with an image element carrying the
/// run's citation (run id, artifact, script, arguments, tree identity). The placeholder is
/// `placeholder`, or the first free one of the slide's layout that takes an image.
///
/// One function for both callers: the agent's `slide.place_figure` turns the result into a
/// slide proposal; the UI's `slide_place_figure` writes it as a user edit.
#[allow(clippy::too_many_arguments)]
pub async fn place_figure(
    runs: &RunService,
    session: &Session,
    repo: &Path,
    svc: &analyst_deck::service::DeckService,
    slide_id: &str,
    run_id: &str,
    artifact: &str,
    placeholder: Option<&str>,
    alt: Option<&str>,
) -> super::ServiceResult<PlacedElement> {
    use super::ServiceError;
    use analyst_deck_protocol::template::ContentKind;
    use serde_json::json;
    let entry = find(runs, session, repo, run_id, artifact).ok_or_else(|| {
        ServiceError::not_found(format!(
            "run `{run_id}` has no artifact `{artifact}`; call artifacts_list to see what its runs produced"
        ))
    })?;
    let ext = entry
        .path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if entry.kind != ArtifactKind::Figure
        || !matches!(ext.as_str(), "png" | "svg" | "jpg" | "jpeg" | "gif")
    {
        return Err(ServiceError::invalid(format!(
            "`{}` is not an image a slide can show (png, svg, jpg, gif); only figures are placed, data stays where the run wrote it",
            entry.path
        )));
    }
    // The rendered file is in the repo (a remote run's figures are pulled there), or in the
    // session's copy of what was pulled.
    let source = crate::util::resolve_in_repo(repo, &entry.path, true)
        .ok()
        .filter(|p| p.is_file())
        .unwrap_or_else(|| session.pulled_artifact_path(run_id, &entry.path));
    let bytes = std::fs::read(&source).map_err(|e| {
        ServiceError::io(format!("the figure `{}` cannot be read: {e}", entry.path))
    })?;
    let id = analyst_deck_protocol::ids::SlideId::new(slide_id.to_string())
        .map_err(|_| ServiceError::invalid("`slide_id` is not a slide id"))?;
    let (slide, hash) = svc
        .ws
        .deck()
        .read_slide(&id)
        .map_err(|e| ServiceError::not_found(e.to_string()))?;
    let mut doc =
        serde_json::to_value(&slide).map_err(|e| ServiceError::internal(e.to_string()))?;
    let key = free_placeholder(
        svc,
        &slide.layout,
        &doc,
        placeholder,
        &[ContentKind::Image],
        false,
    )
    .await?;
    let asset = format!("assets/{}.{ext}", &crate::util::sha256_hex(&bytes)[..16]);
    let target = svc.ws.project.root.join(&asset);
    if !target.is_file() {
        analyst_deck::util::write_atomic(&target, &bytes)
            .map_err(|e| ServiceError::io(format!("writing {asset}: {e}")))?;
    }
    let alt = alt
        .map(str::to_string)
        .or(entry.title.clone())
        .unwrap_or_else(|| format!("Figure from {}", entry.script));
    let element_id = element_id_for(&doc, &key, "e-fig");
    doc["placeholders"][key.as_str()] = json!({
        "type": "image",
        "id": element_id,
        "asset": asset,
        "alt": alt,
        "run_cite": {
            "run_id": entry.run_id,
            "artifact": entry.path,
            "script": entry.script,
            "args": entry.args,
            "tree_hash": entry.tree_hash,
        },
    });
    Ok(PlacedElement { slide: doc, hash })
}

/// A figure of a run placed on a slide.
pub type PlacedFigure = PlacedElement;

/// The placeholder an element goes into: `placeholder` when given, else the first free one of
/// the slide's layout that accepts one of `kinds` (titles skipped unless `titles`).
pub(crate) async fn free_placeholder(
    svc: &analyst_deck::service::DeckService,
    layout: &str,
    doc: &serde_json::Value,
    placeholder: Option<&str>,
    kinds: &[analyst_deck_protocol::template::ContentKind],
    titles: bool,
) -> super::ServiceResult<String> {
    use super::ServiceError;
    use analyst_deck_protocol::template::PlaceholderType;
    if let Some(k) = placeholder {
        return Ok(k.to_string());
    }
    let template = svc.info().await.map_err(ServiceError::from)?.template;
    let candidates: Vec<String> = template
        .masters
        .iter()
        .flat_map(|m| m.layouts.iter())
        .find(|l| l.name == layout)
        .into_iter()
        .flat_map(|l| l.placeholders.iter())
        .filter(|p| p.accepts.iter().any(|a| kinds.contains(a)))
        .filter(|p| {
            titles || !matches!(p.r#type, PlaceholderType::Title | PlaceholderType::CtrTitle)
        })
        .map(|p| p.key.to_string())
        .collect();
    let what = kinds
        .iter()
        .map(|k| format!("{k:?}").to_lowercase())
        .collect::<Vec<_>>()
        .join(" or ");
    candidates
        .iter()
        .find(|k| {
            let cur = &doc["placeholders"][k.as_str()];
            cur.is_null() || cur["type"] == "empty"
        })
        .cloned()
        .ok_or_else(|| {
            ServiceError::invalid(format!(
                "layout {layout:?} has no free placeholder for {what} (candidates: {}); pass `placeholder`, or use a layout with a free one",
                if candidates.is_empty() {
                    "none".to_string()
                } else {
                    candidates.join(", ")
                }
            ))
        })
}

/// A fresh element id with `prefix`, unless the placeholder already holds an element (its id
/// is kept, so citations and locks of the element stay attached).
pub(crate) fn element_id_for(doc: &serde_json::Value, key: &str, prefix: &str) -> String {
    match doc["placeholders"][key]["id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            let used: BTreeSet<String> = doc["placeholders"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .chain(doc["elements"].as_array().into_iter().flatten())
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();
            (1..)
                .map(|n| format!("{prefix}{n}"))
                .find(|id| !used.contains(id))
                .unwrap_or_default()
        }
    }
}
