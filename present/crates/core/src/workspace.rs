//! A deck opened for work: the deck kernel, the reference index, and the deck store behind
//! one handle. The deck tools of the agent loop and the deck service of the UI both go
//! through this. The session that owns the deck says where the kernel's home and the index
//! are; undo is the session's git checkpoints, not a store of its own.

use crate::deck_store::DeckStore;
use crate::index::{ChunkRow, Hit, Index, RefRow};
use crate::kernel::{resolve_runtime_python, KernelClient, KernelError, KernelLaunch};
use crate::project::Project;
use crate::refs::{self, Added};
use crate::util::now_iso;
use analyst_deck_protocol::config::{RefEntry, RefSource};
use analyst_deck_protocol::ids::RefId;
use anyhow::{anyhow, Context};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Options for opening a workspace.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// The app home (`~/.analyst`): the kernel looks for its runtime, rendering engine, and
    /// embedding model under it.
    pub home: PathBuf,
    /// The reference index (SQLite), kept with the session that owns the deck.
    pub index_path: PathBuf,
    pub engine: String,
    pub sandbox: String,
    pub runtime_python: Option<String>,
    pub kernel_env: Vec<(String, String)>,
    /// Environment variables the kernel must not inherit (provider API keys).
    pub env_remove: Vec<String>,
}

impl OpenOptions {
    pub fn new(home: &Path, index_path: &Path) -> OpenOptions {
        OpenOptions {
            home: home.to_path_buf(),
            index_path: index_path.to_path_buf(),
            engine: "auto".into(),
            sandbox: "auto".into(),
            runtime_python: None,
            kernel_env: Vec::new(),
            env_remove: Vec::new(),
        }
    }
}

/// What the checker needs from the project around the deck to verify the provenance of slide
/// elements: the identity of the tree the analysis runs execute (run citations), the content
/// hash of a source file (equation citations), and the recorded value of a run's named result
/// (bound numbers). The session supplies it; without one, provenance is not checked.
pub trait Provenance: Send + Sync {
    /// The identity of the project's working tree now, as runs record it.
    fn tree_identity(&self) -> Option<String>;
    /// SHA-256 (hex) of a repo-relative file's content now; `None` when it does not exist.
    fn file_hash(&self, _path: &str) -> Option<String> {
        None
    }
    /// The named result a run recorded (`{name, value, unit?, uncertainty?}`).
    fn named_result(&self, _run_id: &str, _name: &str) -> Option<Value> {
        None
    }
}

/// A [`Provenance`] that knows only the tree identity.
pub struct TreeOnly(pub std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync>);

impl Provenance for TreeOnly {
    fn tree_identity(&self) -> Option<String> {
        (self.0)()
    }
}

/// The checker rule for a slide element that cites an analysis run whose code has changed.
pub const RULE_STALE: &str = "stale";

pub struct Workspace {
    pub project: Project,
    pub home: PathBuf,
    pub kernel: KernelClient,
    provenance: Mutex<Option<std::sync::Arc<dyn Provenance>>>,
    index: Mutex<Index>,
    /// Held while the shared scratch render is built and read, so a check started by the
    /// user and one started by the agent never read each other's half-written file.
    pub scratch_lock: tokio::sync::Mutex<()>,
}

/// The result of an export.
#[derive(Debug, Clone)]
pub struct Exported {
    pub format: String,
    pub path: String,
    pub errors: usize,
    pub warnings: usize,
    pub engine: Option<String>,
}

impl Workspace {
    pub async fn open(root: &Path, options: &OpenOptions) -> anyhow::Result<Workspace> {
        let project = Project::open(root)?;
        let python = resolve_runtime_python(&options.home, options.runtime_python.as_deref())
            .ok_or_else(|| anyhow!("no deck kernel interpreter found: install the deck runtime or set PRESENT_RUNTIME_PYTHON"))?;
        let launch = KernelLaunch {
            python,
            project: project.root.clone(),
            home: options.home.clone(),
            engine: options.engine.clone(),
            sandbox: options.sandbox.clone(),
            env: options.kernel_env.clone(),
            env_remove: options.env_remove.clone(),
        };
        let kernel = KernelClient::connect(&launch)
            .await
            .context("starting the deck kernel")?;
        if let Some(dir) = options.index_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let index = Index::open(&options.index_path)?;
        let ws = Workspace {
            project,
            home: options.home.clone(),
            kernel,
            provenance: Mutex::new(None),
            index: Mutex::new(index),
            scratch_lock: tokio::sync::Mutex::new(()),
        };
        ws.reindex_missing().await;
        Ok(ws)
    }

    /// The index is per-user state and the references are in the repository: a deck opened
    /// from a fresh clone, or in a new session, indexes the references it finds.
    async fn reindex_missing(&self) {
        let Ok(manifest) = refs::load_manifest(&self.project.root) else {
            return;
        };
        let known: Vec<(String, String)> = self
            .reference_rows()
            .map(|rows| rows.into_iter().map(|r| (r.id, r.version)).collect())
            .unwrap_or_default();
        let mut added = false;
        for entry in &manifest.refs {
            if known
                .iter()
                .any(|(id, v)| id == entry.id.as_str() && v == &entry.version)
            {
                continue;
            }
            match self.ingest(entry).await {
                Ok(v) => {
                    if let Err(e) = self.index_ingested(entry, &v) {
                        tracing::warn!("indexing reference {} failed: {e}", entry.id);
                    }
                    added = true;
                }
                Err(e) => tracing::warn!("reading reference {} failed: {e}", entry.id),
            }
        }
        if added {
            let _ = self.embed_pending().await;
        }
    }

    /// Tell the workspace how to look at the project around the deck, so the checker can
    /// report stale run citations and equations.
    pub fn set_provenance(&self, p: std::sync::Arc<dyn Provenance>) {
        *self.provenance.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    }

    pub fn provenance(&self) -> Option<std::sync::Arc<dyn Provenance>> {
        self.provenance
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Findings for slide elements whose source has changed since they were placed: an
    /// element citing an analysis run (`run_cite`) whose tree identity is not the project's
    /// current one, and an equation (`eq_cite`) whose source file's content hash is not the
    /// file's hash now. Warnings (`stale`); refreshing is the user's call.
    fn stale_findings(&self, slides: Option<&[String]>) -> Vec<Value> {
        let Some(prov) = self.provenance() else {
            return Vec::new();
        };
        let Ok((_, docs)) = self.deck().read_all() else {
            return Vec::new();
        };
        let mut current: Option<Option<String>> = None;
        let mut out = Vec::new();
        for (index, slide) in docs.iter().enumerate() {
            if slides.is_some_and(|ids| !ids.iter().any(|i| i == slide.id.as_str())) {
                continue;
            }
            let Ok(v) = serde_json::to_value(slide) else {
                continue;
            };
            for (pointer, item) in slide_items(&v) {
                let finding = |message: String, fix_hint: &str| {
                    json!({
                        "rule": RULE_STALE,
                        "severity": "warn",
                        "slide_id": slide.id,
                        "slide_index": index,
                        "element_id": item["id"],
                        "pointer": pointer,
                        "message": message,
                        "fix_hint": fix_hint,
                    })
                };
                let cite = &item["run_cite"];
                if let Some(run_tree) = cite["tree_hash"].as_str() {
                    // The tree identity is computed once, and only when a slide cites a run.
                    let now = current.get_or_insert_with(|| prov.tree_identity());
                    if now.as_deref().is_some_and(|n| n != run_tree) {
                        out.push(finding(
                            format!(
                                "the figure cites run {} of {}, and the project's code has changed since that run",
                                cite["run_id"].as_str().unwrap_or("?"),
                                cite["script"].as_str().unwrap_or("?")
                            ),
                            "refresh it to run the script again and place the new figure, or keep the figure if the change does not affect it",
                        ));
                    }
                }
                for (sub, run) in text_runs(item) {
                    let rc = &run["run_cite"];
                    let Some(run_tree) = rc["tree_hash"].as_str() else {
                        continue;
                    };
                    let now = current.get_or_insert_with(|| prov.tree_identity());
                    if now.as_deref().is_some_and(|n| n != run_tree) {
                        let mut f = finding(
                            format!(
                                "the number {:?} is result `{}` of run {} of {}, and the project's code has changed since that run",
                                run["t"].as_str().unwrap_or(""),
                                rc["result_name"].as_str().unwrap_or("?"),
                                rc["run_id"].as_str().unwrap_or("?"),
                                rc["script"].as_str().unwrap_or("?")
                            ),
                            "refresh it to run the script again and update the number",
                        );
                        f["pointer"] = json!(format!("{pointer}{sub}"));
                        out.push(f);
                    }
                }
                let eq = &item["eq_cite"];
                if let (Some(path), Some(hash)) = (eq["path"].as_str(), eq["source_hash"].as_str())
                {
                    let now = prov.file_hash(path);
                    if now.as_deref() != Some(hash) {
                        out.push(finding(
                            format!(
                                "the equation of {} {} since it was placed",
                                eq["qualname"].as_str().unwrap_or("?"),
                                if now.is_some() {
                                    format!("comes from {path}, which has changed")
                                } else {
                                    format!("came from {path}, which no longer exists")
                                }
                            ),
                            "refresh it to extract and check the equation again",
                        ));
                    }
                }
            }
        }
        out
    }

    /// `{"<run id>/<result name>": {name, value, unit?, uncertainty?} | null}` for every named
    /// result a text run of the deck is bound to; `null` when the result is not available.
    /// `None` without a provenance (nothing to compare against).
    pub fn bound_results(&self) -> Option<Value> {
        let mut map = serde_json::Map::new();
        let prov = self.provenance()?;
        let Ok((_, docs)) = self.deck().read_all() else {
            return Some(Value::Object(map));
        };
        for slide in &docs {
            let Ok(v) = serde_json::to_value(slide) else {
                continue;
            };
            for (_, item) in slide_items(&v) {
                for (_, run) in text_runs(item) {
                    let rc = &run["run_cite"];
                    let (Some(id), Some(name)) =
                        (rc["run_id"].as_str(), rc["result_name"].as_str())
                    else {
                        continue;
                    };
                    let key = format!("{id}/{name}");
                    if !map.contains_key(&key) {
                        map.insert(key, prov.named_result(id, name).unwrap_or(Value::Null));
                    }
                }
            }
        }
        Some(Value::Object(map))
    }

    pub async fn close(&self) {
        self.kernel.shutdown().await;
    }

    pub fn deck(&self) -> DeckStore {
        self.project.deck()
    }

    fn with_index<T>(
        &self,
        f: impl FnOnce(&mut Index) -> rusqlite::Result<T>,
    ) -> anyhow::Result<T> {
        let mut guard = self.index.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&mut guard)?)
    }

    pub fn embedding_available(&self) -> bool {
        self.kernel.hello()["embedding"]["kind"]
            .as_str()
            .is_some_and(|k| k != "none")
    }

    // ---- references -----------------------------------------------------------------------

    fn index_ingested(&self, entry: &RefEntry, ingested: &Value) -> anyhow::Result<()> {
        let chunks: Vec<ChunkRow> = ingested["chunks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| ChunkRow {
                        loc: c["loc"].as_str().unwrap_or_default().to_string(),
                        heading_path: c["heading_path"]
                            .as_array()
                            .map(|h| {
                                h.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        text: c["text"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let kind = serde_json::to_value(entry.kind)?
            .as_str()
            .unwrap_or("text")
            .to_string();
        self.with_index(|index| {
            index.upsert_ref(&RefRow {
                id: entry.id.to_string(),
                version: entry.version.clone(),
                kind,
                title: entry.title.clone(),
                file: entry.file.clone(),
                outline_json: ingested["outline"].to_string(),
                status: "ready".into(),
            })?;
            index.replace_chunks(entry.id.as_str(), &entry.version, &chunks)?;
            Ok(())
        })
    }

    /// Copy a file into the project, extract it, and make it searchable. Email attachments
    /// become child references. Returns the reference and any extraction warnings.
    pub async fn add_reference(&self, source: &Path) -> anyhow::Result<(RefEntry, Vec<Value>)> {
        let added = refs::add_file(&self.project.root, source, None)?;
        if let Added::Unchanged(entry) = &added {
            return Ok((entry.clone(), Vec::new()));
        }
        let mut entry = added.entry().clone();
        let ingested = self.ingest(&entry).await?;
        if let Some(title) = ingested["title"].as_str().filter(|t| !t.trim().is_empty()) {
            // Keep the file name for files whose only "title" would be their hash.
            if !title.starts_with(&entry.version) {
                entry.title = title.to_string();
            }
        }
        self.index_ingested(&entry, &ingested)?;
        let mut manifest = refs::load_manifest(&self.project.root)?;
        if let Some(slot) = manifest.refs.iter_mut().find(|r| r.id == entry.id) {
            slot.title = entry.title.clone();
        }
        let mut warnings: Vec<Value> = ingested["warnings"].as_array().cloned().unwrap_or_default();
        for child in ingested["child_refs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let (Some(id), Some(file), Some(version)) = (
                child["ref_id"].as_str(),
                child["file"].as_str(),
                child["version"].as_str(),
            ) else {
                continue;
            };
            let Ok(child_id) = RefId::new(id.to_string()) else {
                continue;
            };
            if manifest.get(&child_id).is_some() {
                continue;
            }
            let Some(kind) = refs::kind_of(Path::new(file)) else {
                continue;
            };
            let child_entry = RefEntry {
                id: child_id,
                file: file.to_string(),
                kind,
                title: child["title"]
                    .as_str()
                    .or(child["name"].as_str())
                    .unwrap_or(id)
                    .to_string(),
                original_path: format!(
                    "{}#{}",
                    entry.original_path,
                    child["name"].as_str().unwrap_or("attachment")
                ),
                imported: now_iso(),
                version: version.to_string(),
                previous_versions: Vec::new(),
                parent: entry.id.to_string(),
                source: RefSource::File,
                analyst: Default::default(),
            };
            manifest.refs.push(child_entry.clone());
            refs::save_manifest(&self.project.root, &manifest)?;
            match self.ingest(&child_entry).await {
                Ok(v) => {
                    self.index_ingested(&child_entry, &v)?;
                    warnings.extend(v["warnings"].as_array().cloned().unwrap_or_default());
                }
                Err(e) => warnings.push(json!({"rule": "refs.child", "severity": "warn", "message": format!("{}: {e}", child_entry.title)})),
            }
        }
        refs::save_manifest(&self.project.root, &manifest)?;
        self.embed_pending().await?;
        Ok((entry, warnings))
    }

    async fn ingest(&self, entry: &RefEntry) -> Result<Value, KernelError> {
        self.kernel
            .call_value(
                "refs.ingest",
                json!({"path": entry.file, "ref_id": entry.id.as_str(), "name": entry.title}),
            )
            .await
    }

    /// Embed every chunk that has no vector yet. A no-op without an embedding model.
    pub async fn embed_pending(&self) -> anyhow::Result<usize> {
        if !self.embedding_available() {
            return Ok(0);
        }
        let mut total = 0;
        loop {
            let batch = self.with_index(|i| i.unembedded(64))?;
            if batch.is_empty() {
                return Ok(total);
            }
            let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();
            let out = self
                .kernel
                .call_value("embed.texts", json!({"texts": texts, "kind": "passage"}))
                .await?;
            let vectors: Vec<Vec<f32>> = serde_json::from_value(out["vectors"].clone())?;
            if vectors.len() != batch.len() {
                return Err(anyhow!(
                    "the embedding model returned {} vectors for {} texts",
                    vectors.len(),
                    batch.len()
                ));
            }
            let ids: Vec<i64> = batch.iter().map(|(id, _)| *id).collect();
            self.with_index(|i| i.set_embeddings(&ids, &vectors))?;
            total += ids.len();
        }
    }

    pub async fn search(
        &self,
        query: &str,
        ref_ids: Option<&[String]>,
        k: usize,
    ) -> anyhow::Result<Vec<Hit>> {
        let vector: Option<Vec<f32>> = if self.embedding_available() {
            match self
                .kernel
                .call_value("embed.texts", json!({"texts": [query], "kind": "query"}))
                .await
            {
                Ok(out) => serde_json::from_value::<Vec<Vec<f32>>>(out["vectors"].clone())
                    .ok()
                    .and_then(|mut v| v.pop()),
                Err(_) => None,
            }
        } else {
            None
        };
        self.with_index(|i| i.search(query, vector.as_deref(), ref_ids, k))
    }

    pub fn reference_rows(&self) -> anyhow::Result<Vec<RefRow>> {
        self.with_index(|i| i.refs())
    }

    pub fn remove_reference(&self, id: &RefId) -> anyhow::Result<()> {
        refs::remove(&self.project.root, id)?;
        self.with_index(|i| i.remove_ref(id.as_str()))
    }

    // ---- build, check, export -------------------------------------------------------------

    /// Build the deck in its current mode into `out` (project-relative).
    pub async fn build(
        &self,
        out: &str,
        snapshot_id: &str,
        slides: Option<&[String]>,
    ) -> Result<Value, KernelError> {
        let deck = self
            .deck()
            .read_deck()
            .map_err(|e| KernelError::Other(e.to_string()))?;
        match deck.mode {
            analyst_deck_protocol::deck::DeckMode::Structured => {
                let mut params = json!({"out": out, "snapshot_id": snapshot_id});
                if let Some(s) = slides {
                    params["slides"] = json!(s);
                }
                self.kernel.call_value("deck.render", params).await
            }
            analyst_deck_protocol::deck::DeckMode::Code => {
                let timeout = self
                    .project
                    .config()
                    .map(|c| c.policy.build_timeout_s)
                    .unwrap_or(60);
                self.kernel.call_value("deck.build", json!({"script": "deck/build.py", "out": out, "snapshot_id": snapshot_id, "timeout_s": timeout})).await
            }
        }
    }

    pub async fn check(
        &self,
        pptx: &str,
        slides: Option<&[String]>,
        rules: Option<&[String]>,
        engine: bool,
    ) -> Result<Vec<Value>, KernelError> {
        let mut params = json!({"pptx": pptx, "engine": engine});
        if let Some(s) = slides {
            params["slides"] = json!(s);
        }
        if let Some(r) = rules {
            params["rules"] = json!(r);
        }
        // The recorded values of the named results that slide text is bound to, for the
        // kernel's number check (`num.bound`).
        if let Some(results) = self.bound_results() {
            params["results"] = results;
        }
        let mut diags = self
            .kernel
            .call_value("check.run", params)
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default();
        if rules.is_none_or(|r| r.iter().any(|x| x == RULE_STALE)) {
            diags.extend(self.stale_findings(slides));
        }
        Ok(diags)
    }

    /// Export the deck. `state_id` names the state the file was built from (the session's
    /// checkpoint id); it is written into the file's properties.
    pub async fn export(
        &self,
        format: &str,
        file_stem: Option<&str>,
        state_id: &str,
    ) -> anyhow::Result<Exported> {
        let deck = self.deck().read_deck()?;
        let stem = file_stem
            .map(str::to_string)
            .unwrap_or_else(|| slug(&deck.title));
        let pptx_rel = format!("exports/{stem}.pptx");
        self.build(&pptx_rel, state_id, None).await?;
        let diags = self.check(&pptx_rel, None, None, false).await?;
        let count = |sev: &str| diags.iter().filter(|d| d["severity"] == sev).count();
        let (errors, warnings) = (count("error"), count("warn"));
        let mut engine = None;
        let path = match format {
            "pptx" => pptx_rel,
            "pdf" => {
                let out = self
                    .kernel
                    .call_value(
                        "render.fidelity",
                        json!({"pptx": pptx_rel, "format": "pdf", "out_dir": "exports"}),
                    )
                    .await?;
                engine = out["engine"].as_str().map(str::to_string);
                out["files"][0].as_str().unwrap_or_default().to_string()
            }
            "png" => {
                let dir = format!("exports/{stem}-png");
                let out = self
                    .kernel
                    .call_value(
                        "render.fidelity",
                        json!({"pptx": pptx_rel, "format": "png", "out_dir": dir, "dpi": 150}),
                    )
                    .await?;
                engine = out["engine"].as_str().map(str::to_string);
                dir
            }
            "md" => {
                let out = format!("exports/{stem}.md");
                self.kernel
                    .call_value(
                        "export.outline_md",
                        json!({"deck": "deck/deck.json", "out": out, "include_sources": true}),
                    )
                    .await?;
                out
            }
            "notes-docx" => {
                let out = format!("exports/{stem}-notes.docx");
                self.kernel
                    .call_value(
                        "export.notes_docx",
                        json!({"deck": "deck/deck.json", "out": out, "include_sources": true}),
                    )
                    .await?;
                out
            }
            other => {
                return Err(anyhow!(
                    "unknown export format {other:?} (pptx, pdf, png, md, notes-docx)"
                ))
            }
        };
        Ok(Exported {
            format: format.to_string(),
            path,
            errors,
            warnings,
            engine,
        })
    }
}

/// A file-name-safe form of a title.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "deck".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

/// Every element of a slide document as JSON with its pointer: placeholders, then free
/// elements' content.
pub fn slide_items(v: &Value) -> Vec<(String, &Value)> {
    let placeholders = v["placeholders"].as_object().into_iter().flat_map(|m| {
        m.iter()
            .map(|(k, item)| (format!("/placeholders/{k}"), item))
    });
    let elements = v["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, el)| (format!("/elements/{i}/content"), &el["content"]));
    placeholders.chain(elements).collect()
}

/// The text runs of an element (a text placeholder, or a free text or shape element), with
/// their pointer below the element.
pub fn text_runs(item: &Value) -> Vec<(String, &Value)> {
    let mut out = Vec::new();
    for (pi, p) in item["text"]["paragraphs"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for (ri, r) in p["runs"].as_array().into_iter().flatten().enumerate() {
            out.push((format!("/text/paragraphs/{pi}/runs/{ri}"), r));
        }
    }
    out
}
