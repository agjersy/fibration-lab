//! Decks of a session (ARCHITECTURE 4): `decks/<slug>/` in the session's repository, each a
//! deck folder (`deck/`, `template/`, `references/`, `assets/`, `exports/`, deck config).
//!
//! [`DeckServices`] is owned by the session runtime (and by the tool context of the agent
//! loop, which shares it). It lists and creates decks without starting anything, and opens a
//! deck's service on first use: that starts the deck kernel (no network) and opens the
//! reference index, which is per-user state under the session store
//! (`~/.analyst/sessions/<id>/decks/<slug>/`). Undo for decks is the session's git
//! checkpoints: deck documents are files in the repository.

use super::error::{ServiceError, ServiceResult};
use super::events::EventHub;
use crate::checkpoint;
use analyst_deck::project::Project;
use analyst_deck::service::{DeckHost, DeckService};
use analyst_deck::workspace::{OpenOptions, Workspace};
use analyst_deck_protocol::deck::DeckMode;
use analyst_protocol::ui::{DeckSummary, SessionEvent};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The directory of the repository that holds the decks.
pub const DECKS_DIR: &str = "decks";

/// A deck slug is a plain folder name: lowercase letters, digits, `-` and `_`, at most 64
/// characters, starting with a letter or digit.
pub fn valid_slug(slug: &str) -> Result<(), String> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && slug.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{slug}` is not a deck name: use lowercase letters, digits, `-` and `_` (at most 64 characters)"
        ))
    }
}

/// A slug from a title (`Q3 review` -> `q3-review`).
pub fn slug_for(title: &str) -> String {
    analyst_deck::workspace::slug(title)
}

/// `decks/<slug>`, repo-relative.
pub fn deck_rel(slug: &str) -> String {
    format!("{DECKS_DIR}/{slug}")
}

/// The absolute deck folder.
pub fn deck_root(repo: &Path, slug: &str) -> PathBuf {
    repo.join(DECKS_DIR).join(slug)
}

/// The repo-relative path of a slide document: what a scope declaration, a protected glob,
/// and a checkpoint see.
pub fn slide_rel(slug: &str, slide_id: &str) -> String {
    format!("{DECKS_DIR}/{slug}/deck/slides/{slide_id}.json")
}

fn summary(repo: &Path, slug: &str, open: bool) -> Option<DeckSummary> {
    let root = deck_root(repo, slug);
    let deck = analyst_deck::deck_store::DeckStore::new(&root)
        .read_deck()
        .ok()?;
    Some(DeckSummary {
        slug: slug.to_string(),
        title: deck.title.clone(),
        mode: match deck.mode {
            DeckMode::Structured => "structured",
            DeckMode::Code => "code",
        }
        .to_string(),
        slides: deck.slide_ids().count() as u32,
        template: deck.template.rsplit('/').next().unwrap_or("").to_string(),
        open,
    })
}

/// The decks in a repository, by slug. Reads `decks/*/deck/deck.json`; starts nothing.
pub fn list(repo: &Path) -> Vec<DeckSummary> {
    let mut slugs: Vec<String> = std::fs::read_dir(repo.join(DECKS_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("deck").join("deck.json").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|s| valid_slug(s).is_ok())
        .collect();
    slugs.sort();
    slugs
        .iter()
        .filter_map(|s| summary(repo, s, false))
        .collect()
}

/// Create a deck from a template file (`.pptx` / `.potx`) under `decks/<slug>/`. The
/// template is copied into the deck, so the deck stays self-contained.
pub fn create(
    repo: &Path,
    slug: &str,
    title: &str,
    template: &Path,
    mode: DeckMode,
) -> ServiceResult<DeckSummary> {
    valid_slug(slug).map_err(ServiceError::invalid)?;
    let root = deck_root(repo, slug);
    if root.join("deck").join("deck.json").exists() {
        return Err(ServiceError::conflict(format!(
            "a deck named `{slug}` already exists"
        )));
    }
    if !template.is_file() {
        return Err(ServiceError::not_found(format!(
            "template `{}` not found",
            template.display()
        )));
    }
    Project::create(&root, title, template, mode)
        .map_err(|e| ServiceError::invalid(format!("{e:#}")))?;
    summary(repo, slug, false).ok_or_else(|| ServiceError::internal("the new deck is unreadable"))
}

/// How deck kernels are started.
#[derive(Debug, Clone)]
pub struct DeckOptions {
    /// Rendering engine preference (`auto`, `powerpoint`, `libreoffice`, `none`).
    pub engine: String,
    /// Sandbox for deck build scripts and skill scripts (`auto`, `native`, `limits`).
    pub sandbox: String,
    /// The interpreter that hosts the deck kernel; `None` resolves it (the bundled runtime
    /// under the home, or the development checkout).
    pub runtime_python: Option<String>,
    /// Extra environment for the deck kernel.
    pub kernel_env: Vec<(String, String)>,
    /// Environment variables the deck kernel must not inherit (provider API keys).
    pub env_remove: Vec<String>,
}

impl Default for DeckOptions {
    fn default() -> Self {
        DeckOptions {
            engine: "auto".into(),
            sandbox: "auto".into(),
            runtime_python: None,
            kernel_env: Vec::new(),
            env_remove: Vec::new(),
        }
    }
}

/// Undo points for a deck are the repository's git checkpoints.
struct GitHost {
    repo: PathBuf,
    slug: String,
}

impl DeckHost for GitHost {
    fn checkpoint(&self, label: &str) -> anyhow::Result<String> {
        let id = checkpoint::new_id(&format!("deck-{}", self.slug));
        checkpoint::create_labeled(&self.repo, &id, label)?;
        Ok(id)
    }

    fn restore_deck(&self, checkpoint: &str) -> anyhow::Result<()> {
        checkpoint::restore_paths(
            &self.repo,
            checkpoint,
            &[format!("{}/deck", deck_rel(&self.slug))],
        )
    }
}

pub struct DeckServices {
    repo: PathBuf,
    home: PathBuf,
    /// `<session dir>/decks`: per-user deck state (reference index, export list).
    state_root: PathBuf,
    options: Mutex<DeckOptions>,
    open: tokio::sync::Mutex<HashMap<String, Arc<DeckService>>>,
    /// Slugs with a running kernel, readable without awaiting.
    opened: Mutex<Vec<String>>,
    events: Mutex<Option<EventHub>>,
}

impl DeckServices {
    pub fn new(repo: &Path, home: &Path, session_dir: &Path) -> Arc<DeckServices> {
        Arc::new(DeckServices {
            repo: repo.to_path_buf(),
            home: home.to_path_buf(),
            state_root: session_dir.join(DECKS_DIR),
            options: Mutex::new(DeckOptions::default()),
            open: tokio::sync::Mutex::new(HashMap::new()),
            opened: Mutex::new(Vec::new()),
            events: Mutex::new(None),
        })
    }

    /// Forward the events of every deck onto a session channel (`SessionEvent::Deck`).
    pub fn set_events(&self, hub: EventHub) {
        if let Ok(mut e) = self.events.lock() {
            *e = Some(hub);
        }
    }

    pub fn set_options(&self, options: DeckOptions) {
        if let Ok(mut o) = self.options.lock() {
            *o = options;
        }
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// Per-user state of one deck in the session store.
    pub fn state_dir(&self, slug: &str) -> PathBuf {
        self.state_root.join(slug)
    }

    fn is_open(&self, slug: &str) -> bool {
        self.opened
            .lock()
            .map(|o| o.iter().any(|s| s == slug))
            .unwrap_or(false)
    }

    pub fn list(&self) -> Vec<DeckSummary> {
        let mut decks = list(&self.repo);
        for d in decks.iter_mut() {
            d.open = self.is_open(&d.slug);
        }
        decks
    }

    /// Whether the repository has any deck (the loop offers deck tools only then).
    pub fn any(&self) -> bool {
        !self.list().is_empty()
    }

    pub fn create(
        &self,
        slug: &str,
        title: &str,
        template: &Path,
        mode: DeckMode,
    ) -> ServiceResult<DeckSummary> {
        let created = create(&self.repo, slug, title, template, mode)?;
        if let Some(hub) = self.events.lock().ok().and_then(|e| e.clone()) {
            hub.emit(SessionEvent::DecksChanged { decks: self.list() });
        }
        Ok(created)
    }

    /// The deck's service, started on first use: the deck kernel (no network), the reference
    /// index, and the event forwarder.
    pub async fn open(&self, slug: &str) -> ServiceResult<Arc<DeckService>> {
        valid_slug(slug).map_err(ServiceError::invalid)?;
        let mut open = self.open.lock().await;
        if let Some(svc) = open.get(slug) {
            return Ok(svc.clone());
        }
        let root = deck_root(&self.repo, slug);
        if !root.join("deck").join("deck.json").is_file() {
            return Err(ServiceError::not_found(format!(
                "no deck named `{slug}` (decks: {})",
                list(&self.repo)
                    .iter()
                    .map(|d| d.slug.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let state_dir = self.state_dir(slug);
        let o = self
            .options
            .lock()
            .map(|o| o.clone())
            .unwrap_or_else(|p| p.into_inner().clone());
        let mut options = OpenOptions::new(&self.home, &state_dir.join("index.sqlite"));
        options.engine = o.engine;
        options.sandbox = o.sandbox;
        options.runtime_python = o.runtime_python;
        options.kernel_env = o.kernel_env;
        options.env_remove = o.env_remove;
        let ws = Workspace::open(&root, &options)
            .await
            .map_err(|e| ServiceError::kernel_offline(format!("opening deck `{slug}`: {e:#}")))?;
        // Run citations on slides are checked against the project's code as it is now.
        let repo = self.repo.clone();
        ws.set_tree_identity(Arc::new(move || super::artifacts::current_tree(&repo)));
        let host = Arc::new(GitHost {
            repo: self.repo.clone(),
            slug: slug.to_string(),
        });
        let svc = DeckService::new(slug, Arc::new(ws), &state_dir, host);
        if let Some(hub) = self.events.lock().ok().and_then(|e| e.clone()) {
            let mut rx = svc.subscribe();
            let deck = slug.to_string();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(e) => hub.emit(SessionEvent::Deck {
                            deck: deck.clone(),
                            event: serde_json::to_value(&e).unwrap_or_default(),
                        }),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            hub.emit(SessionEvent::Resync)
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        open.insert(slug.to_string(), svc.clone());
        if let Ok(mut o) = self.opened.lock() {
            o.push(slug.to_string());
        }
        Ok(svc)
    }

    /// The deck a tool call or command means: the named one, or the only one.
    pub async fn resolve(&self, slug: Option<&str>) -> ServiceResult<Arc<DeckService>> {
        match slug.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => self.open(s).await,
            None => {
                let decks = list(&self.repo);
                match decks.as_slice() {
                    [one] => self.open(&one.slug).await,
                    [] => Err(ServiceError::not_found(
                        "this project has no deck; the user creates one from a template (`analyst deck init`)",
                    )),
                    many => Err(ServiceError::invalid(format!(
                        "this project has several decks; pass `deck` (one of: {})",
                        many.iter()
                            .map(|d| d.slug.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))),
                }
            }
        }
    }

    /// The services that are running, without starting any.
    pub async fn running(&self) -> Vec<Arc<DeckService>> {
        self.open.lock().await.values().cloned().collect()
    }

    /// Files changed outside the deck services (a checkpoint was restored): the open decks
    /// re-read their documents and tell the UI.
    pub async fn refresh(&self, by: &str) {
        for svc in self.running().await {
            let ids: Vec<String> = svc
                .ws
                .deck()
                .read_deck()
                .map(|d| d.slide_ids().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            svc.notify_changed(ids, vec![], by);
        }
    }

    /// Stop every deck kernel.
    pub async fn shutdown(&self) {
        let services: Vec<Arc<DeckService>> =
            self.open.lock().await.drain().map(|(_, s)| s).collect();
        if let Ok(mut o) = self.opened.lock() {
            o.clear();
        }
        for svc in services {
            svc.ws.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_plain_folder_names() {
        for ok in ["q3", "board-2026", "a_b", "0deck"] {
            assert!(valid_slug(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Q3",
            "a b",
            "../x",
            "a/b",
            ".hidden",
            "-x",
            &"x".repeat(65),
        ] {
            assert!(valid_slug(bad).is_err(), "{bad}");
        }
        assert_eq!(slug_for("Q3 Review: margins"), "q3-review-margins");
        assert_eq!(
            slide_rel("q3", "s-abc123"),
            "decks/q3/deck/slides/s-abc123.json"
        );
        assert!(list(Path::new("/nonexistent")).is_empty());
    }
}
