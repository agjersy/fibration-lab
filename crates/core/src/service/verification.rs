//! The verification ledger of a project: `.analyst/verification.jsonl`, append-only and
//! committed with the repository (unless the user ignores it).
//!
//! Each line records the verification state of one item (an equation, a figure or named result
//! of a run, a claim on a slide, an accepted code change) for one content hash. The effective
//! state of an item is the state of the newest entry whose hash is the item's hash NOW; with no
//! such entry it is `unverified`, so any change to the code, a run's output, or a slide resets
//! what was verified about it. Only the user records `human_verified`; the agent can record
//! `machine_checked` (and `flagged`), which is why its tool is allowed under every policy preset.

use super::error::{ServiceError, ServiceResult};
use super::runs::RunService;
use crate::session::Session;
use analyst_protocol::kernel::ArtifactKind;
use analyst_protocol::ui::{
    LedgerEntry, SlideUse, VerificationState, VerificationView, Verifier, VerifyItem, VerifyState,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// The ledger, repo-relative.
pub const LEDGER_REL: &str = ".analyst/verification.jsonl";

/// The longest note kept with an entry.
pub const MAX_NOTE: usize = 2000;

/// The item as one string: `equation:lib/m.py#f`, `figure:<run>/<path>`, `result:<run>/<name>`,
/// `slide_claim:<deck>/<slide>/<element>`, `code_hunk:<proposal>`.
pub fn item_key(item: &VerifyItem) -> String {
    match item {
        VerifyItem::Equation { path, qualname } => format!("equation:{path}#{qualname}"),
        VerifyItem::Figure { run_id, artifact } => format!("figure:{run_id}/{artifact}"),
        VerifyItem::Result { run_id, name } => format!("result:{run_id}/{name}"),
        VerifyItem::SlideClaim {
            deck,
            slide_id,
            element_id,
        } => format!("slide_claim:{deck}/{slide_id}/{element_id}"),
        VerifyItem::CodeHunk { proposal_id } => format!("code_hunk:{proposal_id}"),
    }
}

/// Every entry of the ledger, oldest first; lines that do not parse are skipped.
pub fn read_ledger(repo: &Path) -> Vec<LedgerEntry> {
    let Ok(text) = std::fs::read_to_string(repo.join(LEDGER_REL)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn append(repo: &Path, entry: &LedgerEntry) -> ServiceResult<()> {
    use std::io::Write;
    let path = repo.join(LEDGER_REL);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(entry).map_err(|e| ServiceError::internal(e.to_string()))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

/// What the ledger needs from the session to know items and their content now.
pub struct Scope<'a> {
    pub repo: &'a Path,
    pub runs: &'a RunService,
    pub session: &'a Session,
}

/// One accepted code change from the session's transcripts.
struct Hunk {
    summary: String,
    content: String,
}

/// Per-listing caches: the tree identity, the accepted code changes, the slides.
struct Cache<'a> {
    scope: &'a Scope<'a>,
    tree: Option<Option<String>>,
    hunks: Option<HashMap<String, Hunk>>,
    slides: HashMap<(String, String), Option<Value>>,
}

impl<'a> Cache<'a> {
    fn new(scope: &'a Scope<'a>) -> Cache<'a> {
        Cache {
            scope,
            tree: None,
            hunks: None,
            slides: HashMap::new(),
        }
    }

    fn tree(&mut self) -> Option<String> {
        let repo = self.scope.repo;
        self.tree
            .get_or_insert_with(|| super::artifacts::current_tree(repo))
            .clone()
    }

    fn hunks(&mut self) -> &HashMap<String, Hunk> {
        let session = self.scope.session;
        self.hunks.get_or_insert_with(|| accepted_hunks(session))
    }

    fn slide(&mut self, deck: &str, slide_id: &str) -> Option<Value> {
        let repo = self.scope.repo;
        self.slides
            .entry((deck.to_string(), slide_id.to_string()))
            .or_insert_with(|| {
                if super::decks::valid_slug(deck).is_err()
                    || analyst_deck_protocol::ids::SlideId::new(slide_id.to_string()).is_err()
                {
                    return None;
                }
                let text =
                    std::fs::read_to_string(repo.join(super::decks::slide_rel(deck, slide_id)))
                        .ok()?;
                serde_json::from_str(&text).ok()
            })
            .clone()
    }

    /// The content hash of an item now; `None` when it cannot be read.
    fn hash(&mut self, item: &VerifyItem) -> Option<String> {
        let scope = self.scope;
        match item {
            VerifyItem::Equation { path, .. } => super::provenance::file_hash(scope.repo, path),
            VerifyItem::Figure { run_id, artifact } => {
                let rel = crate::util::normalize_rel_path(artifact).ok()?;
                let local = crate::util::resolve_in_repo(scope.repo, &rel, true)
                    .ok()
                    .filter(|p| p.is_file());
                let pulled = super::figures::safe_run_id(run_id)
                    .then(|| scope.session.pulled_artifact_path(run_id, &rel))
                    .filter(|p| p.is_file());
                match local.or(pulled) {
                    Some(p) => std::fs::read(p).ok().map(|b| crate::util::sha256_hex(&b)),
                    // Not on this machine (a remote figure not pulled yet): what the run reported.
                    None => scope
                        .runs
                        .artifacts(run_id)
                        .into_iter()
                        .find(|a| a.path == rel)
                        .map(|a| {
                            crate::util::sha256_hex(
                                format!("{run_id}\n{}\n{}\n{}", a.path, a.size, a.created_at)
                                    .as_bytes(),
                            )
                        }),
                }
            }
            VerifyItem::Result { run_id, name } => {
                let artifacts = scope.session.artifacts_dir();
                let r = super::artifacts::named_result(
                    scope.runs, scope.repo, &artifacts, run_id, name,
                )?;
                Some(crate::util::sha256_hex(
                    serde_json::to_string(&r).ok()?.as_bytes(),
                ))
            }
            VerifyItem::SlideClaim {
                deck,
                slide_id,
                element_id,
            } => {
                let slide = self.slide(deck, slide_id)?;
                let element = find_element(&slide, element_id)?;
                // The claim rests on the element and on the code it cites: a change to either
                // resets it.
                let mut basis = serde_json::to_string(&element).ok()?;
                if cites_analysis(&element) {
                    basis.push('\n');
                    basis.push_str(&self.tree().unwrap_or_default());
                }
                Some(crate::util::sha256_hex(basis.as_bytes()))
            }
            VerifyItem::CodeHunk { proposal_id } => self
                .hunks()
                .get(proposal_id)
                .map(|h| crate::util::sha256_hex(h.content.as_bytes())),
        }
    }

    fn label(&mut self, item: &VerifyItem) -> String {
        match item {
            VerifyItem::Equation { path, qualname } => format!("{qualname} in {path}"),
            VerifyItem::Figure { run_id, artifact } => format!(
                "{} of run {run_id}",
                artifact.rsplit('/').next().unwrap_or(artifact)
            ),
            VerifyItem::Result { run_id, name } => {
                let artifacts = self.scope.session.artifacts_dir();
                match super::artifacts::named_result(
                    self.scope.runs,
                    self.scope.repo,
                    &artifacts,
                    run_id,
                    name,
                ) {
                    Some(r) => format!(
                        "{name} = {}{} (run {run_id})",
                        r.value,
                        r.unit.map(|u| format!(" {u}")).unwrap_or_default()
                    ),
                    None => format!("{name} of run {run_id}"),
                }
            }
            VerifyItem::SlideClaim {
                deck,
                slide_id,
                element_id,
            } => {
                let what = self
                    .slide(deck, slide_id)
                    .and_then(|s| find_element(&s, element_id))
                    .map(|e| describe_element(&e))
                    .unwrap_or_else(|| "element".into());
                format!("{what} on {deck}/{slide_id}")
            }
            VerifyItem::CodeHunk { proposal_id } => self
                .hunks()
                .get(proposal_id)
                .map(|h| h.summary.clone())
                .unwrap_or_else(|| format!("change {proposal_id}")),
        }
    }
}

/// An element of a slide document by id (placeholder content or a free element's content).
fn find_element(slide: &Value, element_id: &str) -> Option<Value> {
    analyst_deck::workspace::slide_items(slide)
        .into_iter()
        .map(|(_, v)| v)
        .find(|v| v["id"] == element_id)
        .cloned()
}

/// Whether an element cites the analysis: a run's figure, an equation, or a bound number.
fn cites_analysis(element: &Value) -> bool {
    element.get("run_cite").is_some_and(|v| !v.is_null())
        || element.get("eq_cite").is_some_and(|v| !v.is_null())
        || analyst_deck::workspace::text_runs(element)
            .iter()
            .any(|(_, r)| r.get("run_cite").is_some_and(|v| !v.is_null()))
}

fn describe_element(e: &Value) -> String {
    if let Some(eq) = e.get("eq_cite").filter(|v| !v.is_null()) {
        return format!("equation of {}", eq["qualname"].as_str().unwrap_or("?"));
    }
    if let Some(rc) = e.get("run_cite").filter(|v| !v.is_null()) {
        return format!(
            "figure {}",
            rc["artifact"]
                .as_str()
                .and_then(|a| a.rsplit('/').next())
                .unwrap_or("?")
        );
    }
    let bound: Vec<String> = analyst_deck::workspace::text_runs(e)
        .iter()
        .filter(|(_, r)| r.get("run_cite").is_some_and(|v| !v.is_null()))
        .map(|(_, r)| r["t"].as_str().unwrap_or("").to_string())
        .collect();
    if !bound.is_empty() {
        return format!("number {}", bound.join(", "));
    }
    e["type"].as_str().unwrap_or("element").to_string()
}

/// Accepted code proposals (edits, creations, deletions) in the session's transcripts.
fn accepted_hunks(session: &Session) -> HashMap<String, Hunk> {
    use analyst_protocol::transcript::Event;
    let mut proposals: HashMap<String, Hunk> = HashMap::new();
    let mut accepted: Vec<String> = Vec::new();
    let files: Vec<PathBuf> = std::fs::read_dir(session.conversations_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            match serde_json::from_str::<Event>(line) {
                Ok(Event::Proposal {
                    id,
                    kind,
                    summary,
                    diff,
                    targets,
                    ..
                }) if kind.is_file_change() => {
                    let content = format!(
                        "{}\n{}",
                        targets.unwrap_or_default().join("\n"),
                        diff.unwrap_or_default()
                    );
                    proposals.insert(id, Hunk { summary, content });
                }
                Ok(Event::Decision {
                    proposal_id,
                    decision,
                    ..
                }) if decision.is_accepted() => accepted.push(proposal_id),
                _ => {}
            }
        }
    }
    proposals
        .into_iter()
        .filter(|(id, _)| accepted.contains(id))
        .collect()
}

/// Items the queue knows: what slides use (claims and what they cite), what the session's runs
/// produced (figures, named results), accepted code changes, and whatever the ledger names.
fn collect(cache: &mut Cache<'_>) -> BTreeMap<String, (VerifyItem, Vec<SlideUse>)> {
    let scope = cache.scope;
    let mut items: BTreeMap<String, (VerifyItem, Vec<SlideUse>)> = BTreeMap::new();
    let mut add = |item: VerifyItem, used: Option<SlideUse>| {
        let entry = items
            .entry(item_key(&item))
            .or_insert_with(|| (item, Vec::new()));
        if let Some(u) = used {
            if !entry.1.contains(&u) {
                entry.1.push(u);
            }
        }
    };
    for deck in super::decks::list(scope.repo) {
        let store = analyst_deck::deck_store::DeckStore::new(&super::decks::deck_root(
            scope.repo, &deck.slug,
        ));
        let Ok((_, slides)) = store.read_all() else {
            continue;
        };
        for slide in slides {
            let Ok(v) = serde_json::to_value(&slide) else {
                continue;
            };
            cache
                .slides
                .insert((deck.slug.clone(), slide.id.to_string()), Some(v.clone()));
            for (_, el) in analyst_deck::workspace::slide_items(&v) {
                if !cites_analysis(el) {
                    continue;
                }
                let Some(element_id) = el["id"].as_str() else {
                    continue;
                };
                let used = SlideUse {
                    deck: deck.slug.clone(),
                    slide_id: slide.id.to_string(),
                    element_id: element_id.to_string(),
                };
                add(
                    VerifyItem::SlideClaim {
                        deck: deck.slug.clone(),
                        slide_id: slide.id.to_string(),
                        element_id: element_id.to_string(),
                    },
                    Some(used.clone()),
                );
                let rc = &el["run_cite"];
                if let (Some(run_id), Some(artifact)) =
                    (rc["run_id"].as_str(), rc["artifact"].as_str())
                {
                    add(
                        VerifyItem::Figure {
                            run_id: run_id.into(),
                            artifact: artifact.into(),
                        },
                        Some(used.clone()),
                    );
                }
                let eq = &el["eq_cite"];
                if let (Some(path), Some(qualname)) = (eq["path"].as_str(), eq["qualname"].as_str())
                {
                    add(
                        VerifyItem::Equation {
                            path: path.into(),
                            qualname: qualname.into(),
                        },
                        Some(used.clone()),
                    );
                }
                for (_, run) in analyst_deck::workspace::text_runs(el) {
                    let rc = &run["run_cite"];
                    if let (Some(run_id), Some(name)) =
                        (rc["run_id"].as_str(), rc["result_name"].as_str())
                    {
                        add(
                            VerifyItem::Result {
                                run_id: run_id.into(),
                                name: name.into(),
                            },
                            Some(used.clone()),
                        );
                    }
                }
            }
        }
    }
    for a in super::artifacts::list(scope.runs, scope.session, scope.repo, None, None, Some(200)) {
        match a.kind {
            ArtifactKind::Figure => add(
                VerifyItem::Figure {
                    run_id: a.run_id.clone(),
                    artifact: a.path.clone(),
                },
                None,
            ),
            ArtifactKind::Results => {
                for r in a.results.iter().flatten() {
                    add(
                        VerifyItem::Result {
                            run_id: a.run_id.clone(),
                            name: r.name.clone(),
                        },
                        None,
                    );
                }
            }
            _ => {}
        }
    }
    let hunk_ids: Vec<String> = cache.hunks().keys().cloned().collect();
    for id in hunk_ids {
        add(VerifyItem::CodeHunk { proposal_id: id }, None);
    }
    for e in read_ledger(scope.repo) {
        add(e.item, None);
    }
    items
}

fn kind_rank(item: &VerifyItem) -> u8 {
    match item {
        VerifyItem::SlideClaim { .. } => 0,
        VerifyItem::Equation { .. } => 1,
        VerifyItem::Figure { .. } => 2,
        VerifyItem::Result { .. } => 3,
        VerifyItem::CodeHunk { .. } => 4,
    }
}

fn view(
    cache: &mut Cache<'_>,
    ledger: &[LedgerEntry],
    key: String,
    item: VerifyItem,
    on_slides: Vec<SlideUse>,
) -> VerificationView {
    let hash = cache.hash(&item);
    let mine: Vec<&LedgerEntry> = ledger.iter().filter(|e| e.key == key).collect();
    let entry = hash
        .as_ref()
        .and_then(|h| mine.iter().rev().find(|e| &e.hash == h))
        .map(|e| (*e).clone());
    let state = match entry.as_ref().map(|e| e.state) {
        None => VerificationState::Unverified,
        Some(VerifyState::MachineChecked) => VerificationState::MachineChecked,
        Some(VerifyState::HumanVerified) => VerificationState::HumanVerified,
        Some(VerifyState::Flagged) => VerificationState::Flagged,
    };
    VerificationView {
        label: cache.label(&item),
        stale: entry.is_none() && !mine.is_empty(),
        key,
        item,
        state,
        hash,
        entry,
        on_slides,
    }
}

/// The verification queue: every item with its effective state, items used on slides first,
/// then by kind (claims, equations, figures, results, code) and label.
pub fn list(scope: &Scope<'_>) -> Vec<VerificationView> {
    let mut cache = Cache::new(scope);
    let ledger = read_ledger(scope.repo);
    let items = collect(&mut cache);
    let mut out: Vec<VerificationView> = items
        .into_iter()
        .map(|(key, (item, used))| view(&mut cache, &ledger, key, item, used))
        .collect();
    out.sort_by(|a, b| {
        a.on_slides
            .is_empty()
            .cmp(&b.on_slides.is_empty())
            .then(kind_rank(&a.item).cmp(&kind_rank(&b.item)))
            .then(a.label.cmp(&b.label))
    });
    out
}

/// The state of one item now.
pub fn get(scope: &Scope<'_>, item: &VerifyItem) -> VerificationView {
    let mut cache = Cache::new(scope);
    let ledger = read_ledger(scope.repo);
    view(
        &mut cache,
        &ledger,
        item_key(item),
        item.clone(),
        Vec::new(),
    )
}

/// Record a verification state for an item's current content. The agent may record
/// `machine_checked` or `flagged`, never `human_verified`.
pub fn set(
    scope: &Scope<'_>,
    item: &VerifyItem,
    state: VerifyState,
    by: Verifier,
    note: Option<&str>,
) -> ServiceResult<VerificationView> {
    if by == Verifier::Agent && state == VerifyState::HumanVerified {
        return Err(ServiceError::policy_denied(
            "only the user can record human verification; record machine_checked for what you checked",
        ));
    }
    let mut cache = Cache::new(scope);
    let hash = cache.hash(item).ok_or_else(|| {
        ServiceError::not_found(format!(
            "`{}` cannot be read now, so there is nothing to verify",
            item_key(item)
        ))
    })?;
    let note = note
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(MAX_NOTE).collect::<String>());
    let entry = LedgerEntry {
        key: item_key(item),
        item: item.clone(),
        hash,
        state,
        by,
        at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        note,
    };
    append(scope.repo, &entry)?;
    Ok(get(scope, item))
}

/// The items a deck depends on that are not human-verified (the pre-export check): its slide
/// claims and what they cite.
pub fn deck_gaps(scope: &Scope<'_>, deck: &str) -> Vec<VerificationView> {
    list(scope)
        .into_iter()
        .filter(|v| v.on_slides.iter().any(|u| u.deck == deck))
        .filter(|v| v.state != VerificationState::HumanVerified)
        .collect()
}
