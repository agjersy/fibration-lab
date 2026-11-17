//! A project opened for work: the kernel, the index, the deck store, and the snapshot store
//! behind one handle. The agent's tools and the CLI both go through this.

use crate::deck_store::DeckStore;
use crate::index::{ChunkRow, Hit, Index, RefRow};
use crate::kernel::{resolve_runtime_python, KernelClient, KernelError, KernelLaunch};
use crate::project::Project;
use crate::refs::{self, Added};
use crate::snapshots::SnapshotStore;
use crate::util::now_iso;
use anyhow::{anyhow, Context};
use present_protocol::config::{RefEntry, RefSource, SnapshotReason, SnapshotRef};
use present_protocol::ids::{RefId, SnapshotId};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Options for opening a workspace.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub home: PathBuf,
    pub engine: String,
    pub sandbox: String,
    pub runtime_python: Option<String>,
    pub kernel_env: Vec<(String, String)>,
}

impl OpenOptions {
    pub fn new(home: &Path) -> OpenOptions {
        OpenOptions {
            home: home.to_path_buf(),
            engine: "auto".into(),
            sandbox: "auto".into(),
            runtime_python: None,
            kernel_env: Vec::new(),
        }
    }
}

pub struct Workspace {
    pub project: Project,
    pub home: PathBuf,
    pub kernel: KernelClient,
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
    pub snapshot_id: SnapshotId,
    pub errors: usize,
    pub warnings: usize,
    pub engine: Option<String>,
}

impl Workspace {
    pub async fn open(root: &Path, options: &OpenOptions) -> anyhow::Result<Workspace> {
        let project = Project::open(root)?;
        let python = resolve_runtime_python(&options.home, options.runtime_python.as_deref())
            .ok_or_else(|| anyhow!("no kernel interpreter found: install the Present runtime or set PRESENT_RUNTIME_PYTHON"))?;
        let launch = KernelLaunch {
            python,
            project: project.root.clone(),
            home: options.home.clone(),
            engine: options.engine.clone(),
            sandbox: options.sandbox.clone(),
            env: options.kernel_env.clone(),
        };
        let kernel = KernelClient::connect(&launch)
            .await
            .context("starting the deck kernel")?;
        let index = Index::open(&project.index_path(&options.home))?;
        Ok(Workspace {
            project,
            home: options.home.clone(),
            kernel,
            index: Mutex::new(index),
            scratch_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub async fn close(&self) {
        self.kernel.shutdown().await;
    }

    pub fn deck(&self) -> DeckStore {
        self.project.deck()
    }

    pub fn snapshots(&self) -> SnapshotStore {
        SnapshotStore::new(&self.project.root)
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

    /// Import one figure or table from a Personal Analyst repository as a reference. Only
    /// the imported item is copied; the repository is never modified.
    pub async fn add_analyst_reference(
        &self,
        repo_abs: &str,
        item: &str,
    ) -> anyhow::Result<RefEntry> {
        let out = self
            .kernel
            .call_value(
                "analyst.import",
                json!({"repo_abs": repo_abs, "item": item}),
            )
            .await?;
        let text = |k: &str| out[k].as_str().unwrap_or_default().to_string();
        let (file, version) = (text("file"), text("version"));
        if file.is_empty() || version.is_empty() {
            anyhow::bail!("the import returned no file");
        }
        let mut manifest = refs::load_manifest(&self.project.root)?;
        if let Some(existing) = manifest.refs.iter().find(|r| r.version == version) {
            return Ok(existing.clone());
        }
        let kind = refs::kind_of(Path::new(&file))
            .ok_or_else(|| anyhow::anyhow!("unsupported reference format: {file}"))?;
        let origin = present_protocol::config::AnalystOrigin {
            repo: out["analyst"]["repo"]
                .as_str()
                .unwrap_or(repo_abs)
                .to_string(),
            item: out["analyst"]["item"].as_str().unwrap_or(item).to_string(),
            run_id: out["analyst"]["run_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        };
        let original_path = format!("{}/{}", origin.repo.replace('\\', "/"), origin.item);
        let entry = if let Some(slot) = manifest
            .refs
            .iter_mut()
            .find(|r| r.source == RefSource::Analyst && r.original_path == original_path)
        {
            // The same item imported again after a new run: a new version of the reference.
            slot.previous_versions.push(slot.version.clone());
            slot.version = version;
            slot.file = file;
            slot.imported = now_iso();
            slot.analyst = origin;
            slot.clone()
        } else {
            let mut id = RefId::new(text("ref_id")).map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut n = 1;
            while manifest.get(&id).is_some() {
                n += 1;
                id = RefId::new(format!("{}-{n}", text("ref_id")))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            let entry = RefEntry {
                id,
                file,
                kind,
                title: text("title"),
                original_path,
                imported: now_iso(),
                version,
                previous_versions: Vec::new(),
                parent: String::new(),
                source: RefSource::Analyst,
                analyst: origin,
            };
            manifest.refs.push(entry.clone());
            entry
        };
        refs::save_manifest(&self.project.root, &manifest)?;
        let ingested = self.ingest(&entry).await?;
        self.index_ingested(&entry, &ingested)?;
        self.embed_pending().await?;
        Ok(entry)
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

    // ---- snapshots ------------------------------------------------------------------------

    pub fn snapshot(
        &self,
        reason: SnapshotReason,
        turn: Option<&str>,
        label: Option<&str>,
    ) -> anyhow::Result<SnapshotId> {
        let manifest = refs::load_manifest(&self.project.root)?;
        let refs_list = manifest
            .refs
            .iter()
            .map(|r| SnapshotRef {
                id: r.id.clone(),
                version: r.version.clone(),
            })
            .collect();
        let mut assets = Vec::new();
        if let Ok(entries) = std::fs::read_dir(self.project.root.join("assets")) {
            for e in entries.filter_map(|e| e.ok()) {
                if e.path().is_file() {
                    assets.push(crate::util::sha256_file(&e.path())?);
                }
            }
        }
        assets.sort();
        self.snapshots()
            .take(reason, turn, label, assets, refs_list)
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
            present_protocol::deck::DeckMode::Structured => {
                let mut params = json!({"out": out, "snapshot_id": snapshot_id});
                if let Some(s) = slides {
                    params["slides"] = json!(s);
                }
                self.kernel.call_value("deck.render", params).await
            }
            present_protocol::deck::DeckMode::Code => {
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
        Ok(self
            .kernel
            .call_value("check.run", params)
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// Export the deck. Takes an `export` snapshot first so the file is tied to an exact state.
    pub async fn export(&self, format: &str, file_stem: Option<&str>) -> anyhow::Result<Exported> {
        let snapshot_id = self.snapshot(SnapshotReason::Export, None, Some(format))?;
        let deck = self.deck().read_deck()?;
        let stem = file_stem
            .map(str::to_string)
            .unwrap_or_else(|| slug(&deck.title));
        let pptx_rel = format!("exports/{stem}.pptx");
        self.build(&pptx_rel, snapshot_id.as_str(), None).await?;
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
            snapshot_id,
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
