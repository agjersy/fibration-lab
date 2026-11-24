//! The deck service: what the UI's deck commands do to one deck of a session (IPC.md): the
//! slide editor's writes, checks, thumbnails, references, exports, code mode. One
//! [`DeckService`] per deck slug, owned by the session runtime in `analyst-core`, which also
//! supplies the checkpoints that make changes undoable. Shaped after the analysis service.

mod extras;
pub use extras::image_size;

use crate::deck_store::{DeckError, Position};
use crate::kernel::KernelError;
use crate::tools::dispatch::{self, Change, ToolState};
use crate::util::{now_iso, sha256_hex, write_atomic};
use crate::workspace::Workspace;
use analyst_deck_protocol::deck::{DeckMode, Slide};
use analyst_deck_protocol::ids::SlideId;
use analyst_deck_protocol::ui::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

type R<T> = Result<T, IpcError>;

fn err(code: IpcErrorCode, e: impl std::fmt::Display) -> IpcError {
    IpcError::new(code, e.to_string())
}

impl From<DeckError> for IpcError {
    fn from(e: DeckError) -> Self {
        let code = match e {
            DeckError::NotFound(_) => IpcErrorCode::NotFound,
            DeckError::Conflict(_) => IpcErrorCode::Conflict,
            DeckError::Locked(_) => IpcErrorCode::PolicyDenied,
            DeckError::Invalid(_) | DeckError::Json(_) => IpcErrorCode::Invalid,
            DeckError::Io(_) => IpcErrorCode::Io,
        };
        IpcError::new(code, e.to_string())
    }
}

impl From<KernelError> for IpcError {
    fn from(e: KernelError) -> Self {
        let code = match e.code() {
            Some(2000) => IpcErrorCode::NotFound,
            Some(2007) => IpcErrorCode::SandboxUnavailable,
            Some(2008) => IpcErrorCode::EngineUnavailable,
            Some(-32602) | Some(2001) | Some(2002) | Some(2009) | Some(2010) => {
                IpcErrorCode::Invalid
            }
            Some(_) => IpcErrorCode::Internal,
            None => IpcErrorCode::KernelOffline,
        };
        let detail = match &e {
            KernelError::Rpc(rpc) => rpc.data.clone(),
            _ => None,
        };
        IpcError {
            code,
            message: e.to_string(),
            detail,
        }
    }
}

fn internal(e: anyhow::Error) -> IpcError {
    IpcError::internal(format!("{e:#}"))
}

/// What the session that owns a deck provides: undo points. The deck workbench has no
/// history store of its own; decks are files in the session's git repository.
pub trait DeckHost: Send + Sync {
    /// Checkpoint the repository before a change that must be reversible. Returns the
    /// checkpoint id.
    fn checkpoint(&self, label: &str) -> anyhow::Result<String>;
    /// Put the deck's documents (`deck/`) back as they were at a checkpoint.
    fn restore_deck(&self, checkpoint: &str) -> anyhow::Result<()>;
}

/// A host without undo points (tests, one-off commands).
pub struct NoCheckpoints;

impl DeckHost for NoCheckpoints {
    fn checkpoint(&self, _label: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    fn restore_deck(&self, _checkpoint: &str) -> anyhow::Result<()> {
        anyhow::bail!("this deck was opened without checkpoints")
    }
}

#[derive(Default)]
struct RuntimeState {
    diagnostics: Vec<Value>,
    thumbs: BTreeMap<String, (String, String)>,
    /// Renderer-made preview images: slide id to (path, slide hash when made).
    previews: BTreeMap<String, (String, String)>,
    exports: Vec<ExportView>,
    tool_state_template: Option<Value>,
    /// The checkpoint taken before the deck was switched to code generation mode.
    mode_switch_checkpoint: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedState {
    #[serde(default)]
    exports: Vec<ExportView>,
    #[serde(default)]
    mode_switch_checkpoint: Option<String>,
}

pub struct DeckService {
    /// The deck's folder name under `decks/`.
    pub slug: String,
    pub ws: Arc<Workspace>,
    /// Per-user state of this deck in the session store.
    state_dir: PathBuf,
    events: broadcast::Sender<DeckEvent>,
    state: Mutex<RuntimeState>,
    host: Arc<dyn DeckHost>,
    tasks: std::sync::atomic::AtomicU64,
}

impl DeckService {
    pub fn new(
        slug: &str,
        ws: Arc<Workspace>,
        state_dir: &Path,
        host: Arc<dyn DeckHost>,
    ) -> Arc<DeckService> {
        let (events, _) = broadcast::channel(1024);
        let svc = Arc::new(DeckService {
            slug: slug.to_string(),
            ws,
            state_dir: state_dir.to_path_buf(),
            events,
            state: Mutex::new(RuntimeState::default()),
            host,
            tasks: std::sync::atomic::AtomicU64::new(1),
        });
        svc.load_state();
        svc
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DeckEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event: DeckEvent) {
        let _ = self.events.send(event);
    }

    fn state_path(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    fn load_state(&self) {
        let saved: SavedState = std::fs::read(self.state_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut st = self.lock();
        st.exports = saved.exports;
        st.mode_switch_checkpoint = saved.mode_switch_checkpoint;
    }

    fn save_state(&self) {
        let st = self.lock();
        let saved = SavedState {
            exports: st.exports.clone(),
            mode_switch_checkpoint: st.mode_switch_checkpoint.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&saved) {
            let _ = write_atomic(&self.state_path(), &bytes);
        }
    }

    fn next_task(&self) -> String {
        format!(
            "deck-task-{}",
            self.tasks.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    }

    /// The deck changed outside this service (the agent applied a proposal, a checkpoint was
    /// restored): tell the UI and re-check the changed slides.
    pub fn notify_changed(self: &Arc<Self>, changed: Vec<String>, removed: Vec<String>, by: &str) {
        self.lock().tool_state_template = None;
        self.deck_changed(changed, removed, by);
    }

    async fn template(&self) -> R<analyst_deck_protocol::template::TemplateDescription> {
        let cached = self.lock().tool_state_template.clone();
        let value = match cached {
            Some(v) => v,
            None => {
                let deck = self.ws.deck().read_deck()?;
                let v = self
                    .ws
                    .kernel
                    .call_value("template.inspect", json!({"path": deck.template}))
                    .await?;
                self.lock().tool_state_template = Some(v.clone());
                v
            }
        };
        serde_json::from_value(value)
            .map_err(|e| IpcError::internal(format!("template description: {e}")))
    }

    pub async fn info(&self) -> R<DeckInfo> {
        let deck = self.ws.deck().read_deck()?;
        let hello = self.ws.kernel.hello();
        let list = |key: &str| -> Value { hello.get(key).cloned().unwrap_or(Value::Null) };
        Ok(DeckInfo {
            slug: self.slug.clone(),
            title: deck.title.clone(),
            path_abs: self.ws.project.root.to_string_lossy().into_owned(),
            template_name: deck.template.rsplit('/').next().unwrap_or("").to_string(),
            deck_mode: deck.mode,
            slides: deck.slide_ids().count() as u32,
            kernel: KernelState::Ready,
            kernel_network: self.ws.kernel.network_isolation().label().to_string(),
            engines: serde_json::from_value(list("engines")).unwrap_or_default(),
            sandbox: serde_json::from_value(list("sandbox")).ok(),
            embedding: serde_json::from_value(list("embedding")).ok(),
            template: self.template().await?,
            deck,
        })
    }

    // ---- deck ---------------------------------------------------------------------------------

    fn slide_view(&self, slide: Slide, hash: String, index: u32, section: &str) -> SlideView {
        let st = self.lock();
        let id = slide.id.to_string();
        let count = |sev: &str| {
            st.diagnostics
                .iter()
                .filter(|d| d["slide_id"] == id.as_str() && d["severity"] == sev)
                .count() as u32
        };
        let rule = |name: &str| {
            st.diagnostics
                .iter()
                .filter(|d| d["slide_id"] == id.as_str() && d["rule"] == name)
                .count() as u32
        };
        let cited_numbers = cited_numeric_runs(&slide);
        let red = rule("src.numbers");
        let thumb = match st.thumbs.get(&id) {
            Some((path, at_hash)) => Thumb {
                kind: ThumbKind::Fidelity,
                path: Some(path.clone()),
                stale: at_hash != &hash,
            },
            None => match st.previews.get(&id) {
                Some((path, at_hash)) => Thumb {
                    kind: ThumbKind::Preview,
                    path: Some(path.clone()),
                    stale: at_hash != &hash,
                },
                None => Thumb {
                    kind: ThumbKind::Preview,
                    path: None,
                    stale: true,
                },
            },
        };
        SlideView {
            checks: CheckCounts {
                errors: count("error"),
                warnings: count("warn"),
            },
            numbers: NumberCounts {
                green: cited_numbers.saturating_sub(red),
                amber: 0,
                red,
                grey: rule("src.coverage"),
            },
            slide,
            hash,
            index,
            section: section.to_string(),
            thumb,
        }
    }

    pub fn deck_get(&self) -> R<DeckView> {
        let store = self.ws.deck();
        let deck = store.read_deck()?;
        let mut slides = Vec::new();
        let mut index = 0;
        for section in &deck.sections {
            for id in &section.slides {
                let (slide, hash) = store.read_slide(id)?;
                slides.push(self.slide_view(slide, hash, index, &section.title));
                index += 1;
            }
        }
        Ok(DeckView { deck, slides })
    }

    fn deck_changed(self: &Arc<Self>, changed: Vec<String>, removed: Vec<String>, by: &str) {
        if let Ok(deck) = self.ws.deck().read_deck() {
            self.emit(DeckEvent::DeckChanged {
                deck: Box::new(deck),
                changed: changed.clone(),
                removed,
                by: by.into(),
            });
        }
        if !changed.is_empty() {
            let rt = self.clone();
            tokio::spawn(async move {
                let _ = rt.run_checks(Some(changed)).await;
            });
        }
    }

    pub fn slide_update(
        self: &Arc<Self>,
        slide: Slide,
        expected_hash: Option<&str>,
    ) -> R<SlideView> {
        let store = self.ws.deck();
        let hash = store.update_slide(&slide, expected_hash)?;
        let id = slide.id.to_string();
        let deck = store.read_deck()?;
        let (index, section) = locate(&deck, &slide.id);
        let view = self.slide_view(slide, hash, index, &section);
        self.deck_changed(vec![id], vec![], "user");
        Ok(view)
    }

    pub async fn slide_create(self: &Arc<Self>, after: Option<&str>, layout: &str) -> R<SlideView> {
        let store = self.ws.deck();
        let template = self.template().await?;
        if !template
            .masters
            .iter()
            .flat_map(|m| m.layouts.iter())
            .any(|l| l.name == layout)
        {
            return Err(IpcError::invalid(format!(
                "the template has no layout {layout:?}"
            )));
        }
        let id = store.new_slide_id();
        let slide: Slide = serde_json::from_value(json!({"id": id, "kind": "layout", "layout": layout, "placeholders": {}, "elements": []})).map_err(|e| IpcError::internal(e.to_string()))?;
        let after = after
            .map(|a| {
                a.parse::<SlideId>()
                    .map_err(|_| IpcError::invalid("after is not a slide id"))
            })
            .transpose()?;
        let hash = store.create_slide(
            &slide,
            &Position {
                after,
                section: None,
            },
        )?;
        let deck = store.read_deck()?;
        let (index, section) = locate(&deck, &slide.id);
        let view = self.slide_view(slide, hash, index, &section);
        self.deck_changed(vec![id.to_string()], vec![], "user");
        Ok(view)
    }

    pub fn slide_delete(self: &Arc<Self>, slide_id: &str) -> R<()> {
        let id: SlideId = slide_id
            .parse()
            .map_err(|_| IpcError::invalid("not a slide id"))?;
        self.ws.deck().delete_slide(&id)?;
        self.lock().thumbs.remove(slide_id);
        self.deck_changed(vec![], vec![slide_id.to_string()], "user");
        Ok(())
    }

    pub fn slide_duplicate(self: &Arc<Self>, slide_id: &str) -> R<SlideView> {
        let store = self.ws.deck();
        let id: SlideId = slide_id
            .parse()
            .map_err(|_| IpcError::invalid("not a slide id"))?;
        let (original, _) = store.read_slide(&id)?;
        let mut value =
            serde_json::to_value(&original).map_err(|e| IpcError::internal(e.to_string()))?;
        let new_id = store.new_slide_id();
        value["id"] = json!(new_id);
        value["locked"] = Value::Null;
        // Element ids are unique across the deck: give the copy fresh ones.
        let mut text = value.to_string();
        let mut ids = BTreeSet::new();
        for item in value["placeholders"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
            .chain(value["elements"].as_array().into_iter().flatten())
        {
            if let Some(id) = item["id"].as_str() {
                ids.insert(id.to_string());
            }
        }
        for old in ids {
            text = text.replace(
                &format!("\"{old}\""),
                &format!("\"{}\"", crate::util::new_id("e", 6)),
            );
        }
        let mut copy: Value =
            serde_json::from_str(&text).map_err(|e| IpcError::internal(e.to_string()))?;
        if let Some(map) = copy.as_object_mut() {
            map.remove("locked");
        }
        let slide: Slide =
            serde_json::from_value(copy).map_err(|e| IpcError::internal(e.to_string()))?;
        let hash = store.create_slide(
            &slide,
            &Position {
                after: Some(id),
                section: None,
            },
        )?;
        let deck = store.read_deck()?;
        let (index, section) = locate(&deck, &slide.id);
        let view = self.slide_view(slide, hash, index, &section);
        self.deck_changed(vec![new_id.to_string()], vec![], "user");
        Ok(view)
    }

    pub fn slide_move(
        self: &Arc<Self>,
        slide_id: &str,
        after: Option<&str>,
        section: Option<&str>,
    ) -> R<()> {
        let id: SlideId = slide_id
            .parse()
            .map_err(|_| IpcError::invalid("not a slide id"))?;
        let after = after
            .map(|a| {
                a.parse::<SlideId>()
                    .map_err(|_| IpcError::invalid("after is not a slide id"))
            })
            .transpose()?;
        self.ws.deck().move_slide(
            &id,
            &Position {
                after,
                section: section.map(str::to_string),
            },
        )?;
        self.deck_changed(vec![], vec![], "user");
        Ok(())
    }

    pub fn slide_lock(
        self: &Arc<Self>,
        slide_id: &str,
        element_id: Option<&str>,
        locked: bool,
    ) -> R<()> {
        let store = self.ws.deck();
        let id: SlideId = slide_id
            .parse()
            .map_err(|_| IpcError::invalid("not a slide id"))?;
        let (slide, _) = store.read_slide(&id)?;
        let mut value =
            serde_json::to_value(&slide).map_err(|e| IpcError::internal(e.to_string()))?;
        match element_id {
            None => {
                if locked {
                    value["locked"] = json!(true);
                } else if let Some(map) = value.as_object_mut() {
                    map.remove("locked");
                }
            }
            Some(el) => {
                let mut set: BTreeSet<String> = value["locked_elements"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                if locked {
                    set.insert(el.to_string());
                } else {
                    set.remove(el);
                }
                if set.is_empty() {
                    if let Some(map) = value.as_object_mut() {
                        map.remove("locked_elements");
                    }
                } else {
                    value["locked_elements"] = json!(set);
                }
            }
        }
        let updated: Slide =
            serde_json::from_value(value).map_err(|e| IpcError::invalid(e.to_string()))?;
        store.update_slide(&updated, None)?;
        self.deck_changed(vec![], vec![], "user");
        Ok(())
    }

    // ---- checks and thumbnails ----------------------------------------------------------------

    pub async fn run_checks(self: &Arc<Self>, slides: Option<Vec<String>>) -> R<Vec<Value>> {
        let mut scratch = ToolState::default();
        let diags = dispatch::check_slides(&self.ws, &mut scratch, slides.as_deref())
            .await
            .map_err(IpcError::internal)?;
        let affected: Vec<String> = match &slides {
            Some(s) => s.clone(),
            None => self
                .ws
                .deck()
                .read_deck()
                .map(|d| d.slide_ids().map(|s| s.to_string()).collect())
                .unwrap_or_default(),
        };
        {
            let mut st = self.lock();
            st.diagnostics.retain(|d| {
                d["slide_id"]
                    .as_str()
                    .is_some_and(|s| !affected.iter().any(|a| a == s))
                    || (slides.is_some() && d["slide_id"].is_null())
            });
            if slides.is_none() {
                st.diagnostics.clear();
            }
            st.diagnostics.extend(diags.iter().cloned());
        }
        let typed: Vec<analyst_deck_protocol::kernel::Diagnostic> = diags
            .iter()
            .filter_map(|d| serde_json::from_value(d.clone()).ok())
            .collect();
        self.emit(DeckEvent::Diagnostics {
            slides: affected.clone(),
            diagnostics: typed,
        });
        self.emit_views(&affected);
        Ok(diags)
    }

    fn emit_views(&self, ids: &[String]) {
        let store = self.ws.deck();
        let Ok(deck) = store.read_deck() else { return };
        for id in ids {
            let Ok(sid) = id.parse::<SlideId>() else {
                continue;
            };
            if let Ok((slide, hash)) = store.read_slide(&sid) {
                let (index, section) = locate(&deck, &sid);
                self.emit(DeckEvent::SlideView {
                    view: Box::new(self.slide_view(slide, hash, index, &section)),
                });
            }
        }
    }

    /// Render the deck with the fidelity engine and refresh thumbnails.
    pub async fn render_thumbnails(self: &Arc<Self>) -> R<usize> {
        let _scratch = self.ws.scratch_lock.lock().await;
        dispatch::build_agent_deck(&self.ws)
            .await
            .map_err(IpcError::internal)?;
        let out = self.ws.kernel.call_value("render.fidelity", json!({"pptx": dispatch::AGENT_PPTX, "format": "png", "dpi": 72, "out_dir": ".present/cache/thumbs/deck"})).await?;
        let files: Vec<String> = out["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str().map(str::to_string))
            .collect();
        let store = self.ws.deck();
        let deck = store.read_deck()?;
        let mut visible = Vec::new();
        for id in deck.slide_ids() {
            let (slide, hash) = store.read_slide(id)?;
            if slide.hidden != Some(true) {
                visible.push((id.to_string(), hash));
            }
        }
        {
            let mut st = self.lock();
            for ((id, hash), file) in visible.iter().zip(&files) {
                st.thumbs.insert(id.clone(), (file.clone(), hash.clone()));
            }
        }
        let ids: Vec<String> = visible.into_iter().map(|(id, _)| id).collect();
        self.emit_views(&ids);
        Ok(files.len())
    }

    /// Run `f` as a background task reporting through `task_progress` and `task_done`.
    pub fn spawn_task<F, Fut>(self: &Arc<Self>, label: &str, f: F) -> String
    where
        F: FnOnce(Arc<DeckService>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = R<Option<String>>> + Send + 'static,
    {
        let task_id = self.next_task();
        let rt = self.clone();
        let id = task_id.clone();
        self.emit(DeckEvent::TaskProgress {
            task_id: id.clone(),
            fraction: None,
            message: label.to_string(),
        });
        tokio::spawn(async move {
            let outcome = f(rt.clone()).await;
            match outcome {
                Ok(message) => rt.emit(DeckEvent::TaskDone {
                    task_id: id,
                    ok: true,
                    message,
                }),
                Err(e) => rt.emit(DeckEvent::TaskDone {
                    task_id: id,
                    ok: false,
                    message: Some(e.message),
                }),
            }
        });
        task_id
    }

    // ---- references ---------------------------------------------------------------------------

    pub fn refs_list(&self) -> R<Vec<RefView>> {
        let manifest = crate::refs::load_manifest(&self.ws.project.root).map_err(internal)?;
        let rows = self.ws.reference_rows().map_err(internal)?;
        let (_, slides) = self.ws.deck().read_all().unwrap_or_else(|_| {
            (
                self.ws.deck().read_deck().unwrap_or_else(|_| empty_deck()),
                Vec::new(),
            )
        });
        let mut cited: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for slide in &slides {
            let text = serde_json::to_string(slide).unwrap_or_default();
            for r in &manifest.refs {
                if text.contains(&format!("ref:{}@", r.id)) {
                    cited
                        .entry(r.id.to_string())
                        .or_default()
                        .insert(slide.id.to_string());
                }
            }
        }
        Ok(manifest
            .refs
            .iter()
            .map(|r| {
                let row = rows.iter().find(|x| x.id == r.id.as_str());
                RefView {
                    id: r.id.to_string(),
                    version: r.version.clone(),
                    kind: r.kind,
                    title: r.title.clone(),
                    size_bytes: std::fs::metadata(self.ws.project.root.join(&r.file))
                        .map(|m| m.len())
                        .unwrap_or(0),
                    pages: None,
                    status: row
                        .map(|x| x.status.clone())
                        .unwrap_or_else(|| "queued".into()),
                    error: None,
                    cited_by: cited
                        .get(r.id.as_str())
                        .map(|s| s.iter().cloned().collect())
                        .unwrap_or_default(),
                    parent: if r.parent.is_empty() {
                        None
                    } else {
                        Some(r.parent.clone())
                    },
                }
            })
            .collect())
    }

    pub fn refs_add(self: &Arc<Self>, paths_abs: Vec<String>) -> String {
        self.spawn_task("Adding references", move |rt| async move {
            let mut failed = Vec::new();
            for path in &paths_abs {
                if let Err(e) = rt.ws.add_reference(Path::new(path)).await {
                    failed.push(format!("{path}: {e:#}"));
                }
                if let Ok(refs) = rt.refs_list() {
                    rt.emit(DeckEvent::RefsChanged { refs });
                }
            }
            if failed.is_empty() {
                Ok(Some(format!("{} added", paths_abs.len())))
            } else {
                Err(IpcError::invalid(failed.join("\n")))
            }
        })
    }

    pub fn refs_remove(&self, ref_id: &str) -> R<()> {
        if self
            .refs_list()?
            .iter()
            .any(|r| r.id == ref_id && !r.cited_by.is_empty())
        {
            return Err(IpcError::invalid(
                "the reference is cited by slides; remove the citations first",
            ));
        }
        if !self.refs_list()?.iter().any(|r| r.id == ref_id) {
            return Err(IpcError::not_found(format!("no reference {ref_id}")));
        }
        self.ws
            .remove_reference(
                &ref_id
                    .parse()
                    .map_err(|_| IpcError::invalid("not a reference id"))?,
            )
            .map_err(internal)?;
        self.emit(DeckEvent::RefsChanged {
            refs: self.refs_list()?,
        });
        Ok(())
    }

    pub fn refs_remove_unused(&self) -> R<Vec<String>> {
        let unused: Vec<String> = self
            .refs_list()?
            .into_iter()
            .filter(|r| r.cited_by.is_empty() && r.parent.is_none())
            .map(|r| r.id)
            .collect();
        for id in &unused {
            if let Ok(rid) = id.parse() {
                self.ws.remove_reference(&rid).map_err(internal)?;
            }
        }
        self.emit(DeckEvent::RefsChanged {
            refs: self.refs_list()?,
        });
        Ok(unused)
    }

    pub async fn refs_search(
        &self,
        query: &str,
        ref_ids: Option<Vec<String>>,
    ) -> R<Vec<SearchHit>> {
        let hits = self
            .ws
            .search(query, ref_ids.as_deref(), 12)
            .await
            .map_err(internal)?;
        Ok(hits
            .into_iter()
            .map(|h| SearchHit {
                cite: h.cite(),
                title: h.title,
                heading_path: h.heading_path,
                snippet: h.snippet,
            })
            .collect())
    }

    /// The passage behind a citation, for the source lens.
    pub async fn source_passage(&self, cite: &str) -> R<SourcePassage> {
        let ref_id = cite
            .strip_prefix("ref:")
            .and_then(|r| r.split('@').next())
            .ok_or_else(|| IpcError::invalid("not a citation"))?;
        let loc = cite.split_once('#').map(|(_, l)| l).unwrap_or("");
        let manifest = crate::refs::load_manifest(&self.ws.project.root).map_err(internal)?;
        let entry = manifest
            .refs
            .iter()
            .find(|r| r.id.as_str() == ref_id)
            .ok_or_else(|| {
                IpcError::not_found(format!("reference {ref_id} is not in this project"))
            })?;
        let kind_name = serde_json::to_value(entry.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let tabular = matches!(kind_name.as_str(), "xlsx" | "csv" | "analyst-table")
            || loc.rsplit('/').next().is_some_and(|l| {
                l.starts_with('t') && l[1..].chars().all(|c| c.is_ascii_digit()) && l.len() > 1
            });
        let mut passage = SourcePassage {
            cite: cite.to_string(),
            ref_title: entry.title.clone(),
            kind: entry.kind,
            text: None,
            highlight: None,
            page_png: None,
            table: None,
            highlight_cells: None,
        };
        if tabular {
            let table = self
                .ws
                .kernel
                .call_value("refs.table", json!({"cite": cite}))
                .await?;
            let parsed: analyst_deck_protocol::deck::Table =
                serde_json::from_value(table).map_err(|e| IpcError::internal(e.to_string()))?;
            passage.highlight_cells =
                Some((0, 0, parsed.rows.len() as u32, parsed.columns.len() as u32));
            passage.table = Some(parsed);
        } else if kind_name == "image" || kind_name == "analyst-figure" {
            passage.page_png = Some(entry.file.clone());
        } else {
            let out = self
                .ws
                .kernel
                .call_value("refs.read", json!({"cite": cite, "max_chars": 6000}))
                .await?;
            let text = out["text"].as_str().unwrap_or_default().to_string();
            passage.highlight = Some((0, text.chars().count() as u32));
            passage.text = Some(text);
            if kind_name == "pdf" {
                if let Some(page) = loc
                    .strip_prefix('p')
                    .and_then(|l| l.split([':', '/']).next())
                    .and_then(|n| n.parse::<u64>().ok())
                {
                    if let Ok(img) = self
                        .ws
                        .kernel
                        .call_value(
                            "refs.page_image",
                            json!({"ref_id": ref_id, "page": page, "dpi": 96}),
                        )
                        .await
                    {
                        passage.page_png = img["png"].as_str().map(str::to_string);
                    }
                }
            }
        }
        Ok(passage)
    }

    // ---- exports ----

    pub fn exports_list(&self) -> Vec<ExportView> {
        self.lock().exports.clone()
    }

    pub fn export_run(self: &Arc<Self>, format: String) -> String {
        self.spawn_task(&format!("Exporting {format}"), move |rt| async move {
            Ok(Some(rt.export(&format).await?.path))
        })
    }

    /// Export the deck now (the command line waits for it; the UI uses [`Self::export_run`]).
    /// The export is tied to an exact state: the checkpoint taken just before it.
    pub async fn export(self: &Arc<Self>, format: &str) -> R<ExportView> {
        let checkpoint = self
            .host
            .checkpoint(&format!("export {format}"))
            .map_err(internal)?;
        let exported = self
            .ws
            .export(format, None, &checkpoint)
            .await
            .map_err(|e| {
                let text = format!("{e:#}");
                if text.contains("engine") {
                    err(IpcErrorCode::EngineUnavailable, text)
                } else {
                    IpcError::internal(text)
                }
            })?;
        let view = ExportView {
            id: crate::util::new_id("x", 10),
            format: exported.format,
            preset: None,
            path: exported.path,
            checkpoint,
            created_at: now_iso(),
            errors: exported.errors as u32,
            warnings: exported.warnings as u32,
            engine: exported.engine,
        };
        self.lock().exports.insert(0, view.clone());
        self.save_state();
        self.emit(DeckEvent::ExportDone {
            export: view.clone(),
        });
        Ok(view)
    }

    pub fn export_path(&self, export_id: &str) -> R<PathBuf> {
        let st = self.lock();
        let view = st
            .exports
            .iter()
            .find(|e| e.id == export_id)
            .ok_or_else(|| IpcError::not_found("no such export"))?;
        Ok(self.ws.project.root.join(&view.path))
    }

    // ---- code mode ----------------------------------------------------------------------------

    pub fn code_read(&self, path: &str) -> R<FileContent> {
        let rel = safe_code_path(path)?;
        let content = std::fs::read_to_string(self.ws.project.root.join(&rel))
            .map_err(|_| IpcError::not_found(format!("{rel} does not exist")))?;
        Ok(FileContent {
            hash: sha256_hex(content.as_bytes()),
            path: rel,
            content,
        })
    }

    pub fn code_write(
        self: &Arc<Self>,
        path: &str,
        content: &str,
        expected_hash: Option<&str>,
    ) -> R<WriteResult> {
        let rel = safe_code_path(path)?;
        let full = self.ws.project.root.join(&rel);
        if let (Some(expected), Ok(current)) = (expected_hash, std::fs::read(&full)) {
            if sha256_hex(&current) != expected {
                return Err(IpcError::new(
                    IpcErrorCode::Conflict,
                    format!("{rel} changed since it was read"),
                ));
            }
        }
        write_atomic(&full, content.as_bytes()).map_err(|e| err(IpcErrorCode::Io, e))?;
        Ok(WriteResult {
            hash: sha256_hex(content.as_bytes()),
        })
    }

    pub fn deck_build(self: &Arc<Self>) -> String {
        self.spawn_task("Building the deck", |rt| async move {
            let built = {
                let _scratch = rt.ws.scratch_lock.lock().await;
                rt.ws.build(dispatch::AGENT_PPTX, "", None).await
            };
            match built {
                Ok(out) => {
                    for (stream, key) in [("stdout", "stdout"), ("stderr", "stderr")] {
                        if let Some(text) = out[key].as_str().filter(|t| !t.is_empty()) {
                            rt.emit(DeckEvent::BuildOutput {
                                stream: stream.into(),
                                chunk: text.to_string(),
                            });
                        }
                    }
                    let _ = rt.run_checks(None).await;
                    Ok(Some("built".into()))
                }
                Err(e) => {
                    for key in ["stdout", "stderr"] {
                        if let Some(text) = e
                            .data(key)
                            .and_then(|v| v.as_str())
                            .filter(|t| !t.is_empty())
                        {
                            rt.emit(DeckEvent::BuildOutput {
                                stream: key.into(),
                                chunk: text.to_string(),
                            });
                        }
                    }
                    Err(e.into())
                }
            }
        })
    }

    /// Turn the structured deck into an equivalent build script (code generation mode). The
    /// repository is checkpointed first, so the switch can be undone
    /// (`deck_switch_mode(structured, via: "revert")`, or any checkpoint restore).
    pub async fn switch_to_code(self: &Arc<Self>) -> R<Value> {
        let checkpoint = self
            .host
            .checkpoint("before switching the deck to code mode")
            .map_err(internal)?;
        let mut scratch = ToolState::default();
        let out = dispatch::apply(
            &self.ws,
            &mut scratch,
            &Change::ModeSwitch {
                reason: "requested".into(),
            },
        )
        .await
        .map_err(IpcError::invalid)?;
        {
            let mut st = self.lock();
            st.mode_switch_checkpoint = Some(checkpoint);
            st.tool_state_template = None;
        }
        self.save_state();
        Ok(out)
    }

    pub fn deck_switch_mode(self: &Arc<Self>, to: DeckMode, via: Option<String>) -> String {
        self.spawn_task("Switching deck mode", move |rt| async move {
            let current = rt.ws.deck().read_deck()?.mode;
            match (current, to) {
                (DeckMode::Structured, DeckMode::Code) => {
                    rt.switch_to_code().await?;
                }
                (DeckMode::Code, DeckMode::Structured) => {
                    if via.as_deref() != Some("revert") {
                        return Err(IpcError::invalid("switching back needs via: \"revert\" (importing a built deck is not available yet)"));
                    }
                    let before = rt
                        .lock()
                        .mode_switch_checkpoint
                        .clone()
                        .filter(|c| !c.is_empty())
                        .ok_or_else(|| {
                            IpcError::not_found(
                                "no checkpoint from before the switch to code mode",
                            )
                        })?;
                    rt.host.restore_deck(&before).map_err(internal)?;
                }
                _ => return Ok(Some("already in that mode".into())),
            }
            rt.lock().tool_state_template = None;
            if let Ok(info) = rt.info().await {
                rt.emit(DeckEvent::InfoChanged { info: Box::new(info) });
            }
            let ids: Vec<String> = rt.ws.deck().read_deck()?.slide_ids().map(|s| s.to_string()).collect();
            rt.deck_changed(ids, vec![], "user");
            Ok(None)
        })
    }
}

fn empty_deck() -> analyst_deck_protocol::deck::Deck {
    analyst_deck_protocol::deck::Deck {
        schema_version: analyst_deck_protocol::SCHEMA_VERSION,
        mode: DeckMode::Structured,
        title: String::new(),
        author: None,
        template: String::new(),
        slide_size: None,
        sections: Vec::new(),
        footer: None,
        classification: None,
        brief: None,
        properties: None,
    }
}

fn locate(deck: &analyst_deck_protocol::deck::Deck, id: &SlideId) -> (u32, String) {
    let mut index = 0;
    for section in &deck.sections {
        for s in &section.slides {
            if s == id {
                return (index, section.title.clone());
            }
            index += 1;
        }
    }
    (index, String::new())
}

fn cited_numeric_runs(slide: &Slide) -> u32 {
    let v = serde_json::to_value(slide).unwrap_or(Value::Null);
    let mut n = 0;
    for item in v["placeholders"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        for para in item["text"]["paragraphs"].as_array().into_iter().flatten() {
            for run in para["runs"].as_array().into_iter().flatten() {
                let cited = run["cite"].as_array().is_some_and(|a| !a.is_empty());
                if cited
                    && run["t"]
                        .as_str()
                        .is_some_and(|t| t.chars().any(|c| c.is_ascii_digit()))
                {
                    n += 1;
                }
            }
        }
    }
    n
}

fn safe_code_path(path: &str) -> R<String> {
    let p = path.replace('\\', "/");
    if p.starts_with('/')
        || p.split('/').any(|c| c == ".." || c.is_empty())
        || !p.starts_with("deck/")
        || !p.ends_with(".py")
    {
        return Err(IpcError::invalid(
            "only Python files under deck/ can be read or written here",
        ));
    }
    Ok(p)
}
