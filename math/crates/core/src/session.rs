//! A scene session (ARCHITECTURE 7): one open `scene.py`, its engine, and the one path by
//! which the scene changes.
//!
//! Whoever asks for a change (the agent, a chip, a slider release, the cell editor), it becomes
//! an edit to the notebook: the candidate notebook is replayed from the top, and only when
//! every cell runs is it written to disk and kept. A candidate that fails leaves the file, the
//! notebook and the engine's scene exactly as they were, and the caller gets the failing
//! cell's error. Undo restores the previous file text and replays it.
//!
//! The file on disk is the source of truth. Before each edit the session re-reads it, so a
//! cell changed in an external editor is picked up rather than overwritten.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use math_protocol::{CellResult, CellStatus, ReplayResult, RunnerCell};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::chips::{self, Chip, ChipKind};
use crate::engine::{EngineClient, EngineConfig};
use crate::notebook::Notebook;
use crate::replay::{finish, replay_keeping, KeptRunner, ReplayOptions, PREAMBLE_ID};
use crate::router::{Router, RouterHandle};

/// Undo steps kept per session.
const UNDO_LIMIT: usize = 200;

/// How a session is opened.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub engine: EngineConfig,
    pub replay: ReplayOptions,
    /// Where the attach socket is created (its directory is made 0700).
    pub socket: PathBuf,
    /// JSONL log of every call that reaches the engine through the router.
    pub call_log: Option<PathBuf>,
}

/// One change to the notebook.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum CellEdit {
    Append {
        code: String,
        caption: String,
    },
    InsertAfter {
        after: Option<String>,
        code: String,
        caption: String,
    },
    Replace {
        id: String,
        code: String,
        caption: Option<String>,
    },
    Delete {
        id: String,
    },
}

/// The outcome of an edit.
#[derive(Clone, Debug, Serialize)]
pub struct Applied {
    /// Whether the edit was kept. When false nothing changed.
    pub ok: bool,
    /// The cell the edit created or changed (`None` for a delete).
    pub cell: Option<String>,
    /// The replay of the candidate notebook: the new scene when `ok`, else the failure.
    pub replay: ReplayResult,
    /// Whether cells were run to get there. False when the change was applied to the live
    /// scene because nothing else depended on it; `replay` is then what a replay would give.
    pub replayed: bool,
}

impl Applied {
    /// The failing cell's result, when the edit was refused.
    pub fn failure(&self) -> Option<&math_protocol::CellResult> {
        self.replay
            .results
            .iter()
            .find(|r| r.status == CellStatus::Error)
    }
}

pub struct SceneSession {
    path: PathBuf,
    notebook: Notebook,
    engine: Arc<EngineClient>,
    router: RouterHandle,
    opts: ReplayOptions,
    undo: Vec<String>,
    last: ReplayResult,
    /// Operation name to purity, from the engine's manifest.
    pure_ops: HashMap<String, bool>,
    /// The runner that holds the namespace of the kept notebook's cells, when every cell ran.
    runner: Option<KeptRunner>,
    /// Cells run alone since the last replay from the top (see `verify`).
    unverified: usize,
    /// A replay of the unchanged notebook that did not give the scene the session had,
    /// waiting to be reported by `verify`.
    mismatch: Option<Verified>,
}

/// The outcome of `verify`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Verified {
    /// How many cells had been run alone since the last replay from the top.
    pub checked: usize,
    /// Whether the replay from the top gave the scene the session had.
    pub consistent: bool,
    /// The hash the session had, and the hash the replay gave.
    pub had: String,
    pub replayed: String,
}

impl SceneSession {
    /// Open (or create) the scene at `path`, start its engine and router, and replay it.
    pub async fn open(path: &Path, config: SessionConfig) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let fresh = Notebook::new().print();
                write_atomic(path, &fresh)?;
                fresh
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let notebook =
            Notebook::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
        let engine = Arc::new(EngineClient::spawn(&config.engine).await?);
        let router = Arc::new(Router::new(
            Arc::clone(&engine),
            config.call_log.as_deref(),
        )?);
        let handle = router.serve(&config.socket)?;
        let manifest = engine.describe_api(false).await?;
        let pure_ops = manifest["operations"]
            .as_array()
            .map(|ops| {
                ops.iter()
                    .filter_map(|o| Some((o["name"].as_str()?.to_string(), o["pure"].as_bool()?)))
                    .collect()
            })
            .unwrap_or_default();
        let mut opts = config.replay;
        if opts.cwd.is_none() {
            opts.cwd = path.parent().map(Path::to_path_buf);
        }
        let (last, runner) = replay_keeping(&handle, &notebook, &opts).await?;
        Ok(Self {
            path: path.to_path_buf(),
            notebook,
            engine,
            router: handle,
            opts,
            undo: Vec::new(),
            last,
            pure_ops,
            runner,
            unverified: 0,
            mismatch: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn notebook(&self) -> &Notebook {
        &self.notebook
    }

    /// The result of the latest replay of the kept notebook (the current scene).
    pub fn last(&self) -> &ReplayResult {
        &self.last
    }

    /// The current scene state (the engine's `state.get` shape).
    pub fn state(&self) -> &Value {
        &self.last.state
    }

    pub fn isolation(&self) -> &str {
        &self.engine.isolation
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether `fn_name` is a pure engine operation; `None` when there is no such operation.
    pub fn is_pure(&self, fn_name: &str) -> Option<bool> {
        self.pure_ops.get(fn_name).copied()
    }

    /// Pick up a change made to the file outside the session. Returns whether it changed.
    /// A file that no longer parses is an error and the session keeps its notebook.
    pub async fn sync_from_disk(&mut self) -> Result<bool> {
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        if text == self.notebook.print() {
            return Ok(false);
        }
        let notebook = Notebook::parse(&text)
            .with_context(|| format!("{} was changed and no longer parses", self.path.display()))?;
        self.push_undo(self.notebook.print());
        self.notebook = notebook;
        self.replay_kept().await?;
        Ok(true)
    }

    /// Apply one edit: replay the candidate, keep it only if every cell runs.
    pub async fn apply(&mut self, edit: CellEdit) -> Result<Applied> {
        self.sync_from_disk().await?;
        let mut candidate = self.notebook.clone();
        let cell = match &edit {
            CellEdit::Append { code, caption } => Some(candidate.append(code, caption)?),
            CellEdit::InsertAfter {
                after,
                code,
                caption,
            } => Some(candidate.insert_after(after.as_deref(), code, caption)?),
            CellEdit::Replace { id, code, caption } => {
                candidate.replace(id, code, caption.as_deref())?;
                Some(id.clone())
            }
            CellEdit::Delete { id } => {
                candidate.delete(id)?;
                None
            }
        };
        let appended = matches!(&cell, Some(id) if candidate.cells.last().map(|c| &c.id) == Some(id))
            && candidate.cells.len() == self.notebook.cells.len() + 1;
        if appended {
            if let Some(applied) = self.append_directly(&candidate).await? {
                return Ok(applied);
            }
        }
        let (result, runner) = replay_keeping(&self.router, &candidate, &self.opts).await?;
        self.unverified = 0;
        if !result.ok() {
            // Put the engine's scene back to the kept notebook's.
            self.replay_kept().await?;
            return Ok(Applied {
                ok: false,
                cell,
                replay: result,
                replayed: true,
            });
        }
        let before = self.notebook.print();
        write_atomic(&self.path, &candidate.print())?;
        self.push_undo(before);
        self.notebook = candidate;
        self.last = result.clone();
        self.runner = runner;
        Ok(Applied {
            ok: true,
            cell,
            replay: result,
            replayed: true,
        })
    }

    /// Replay the kept notebook from the top: the scene, the runner and `last` are what the
    /// file on disk defines.
    async fn replay_kept(&mut self) -> Result<()> {
        self.runner = None;
        let (last, runner) = replay_keeping(&self.router, &self.notebook, &self.opts).await?;
        self.last = last;
        self.runner = runner;
        self.unverified = 0;
        Ok(())
    }

    /// Replay the kept notebook after a cell that was tried alone did not work out. The
    /// notebook is the one the session's scene came from, so this replay is also the check
    /// of the cells run alone before: a different scene is kept for `verify` to report.
    async fn replay_kept_checked(&mut self) -> Result<()> {
        let checked = self.unverified;
        let had = self.last.hash.clone();
        self.replay_kept().await?;
        if checked > 0 && (self.last.hash != had || !self.last.ok()) {
            self.mismatch = Some(Verified {
                checked,
                consistent: false,
                had,
                replayed: self.last.hash.clone(),
            });
        }
        Ok(())
    }

    /// Run the cells of `candidate` that the kept notebook does not have, alone, in the kept
    /// runner (ARCHITECTURE 5.6). `candidate` is the kept notebook with one cell added at
    /// the end, or with its last cell replaced after the scene was put back to before it.
    ///
    /// Returns the result with the earlier cells' results in front, as a replay reports
    /// them, or `None` when there is no usable runner. After a failed cell, or when the
    /// runner breaks, the runner is gone: the caller must replay from the top.
    async fn run_last_cell(&mut self, candidate: &Notebook) -> Result<Option<ReplayResult>> {
        if !self.last.ok() {
            return Ok(None);
        }
        let Some(mut runner) = self.runner.take() else {
            return Ok(None);
        };
        let Some(cell) = candidate.cells.last() else {
            return Ok(None);
        };
        let reads_before = self.router.router().state_reads();
        let request = vec![RunnerCell {
            id: cell.id.clone(),
            code: cell.code.clone(),
        }];
        let output = match runner.run(request, self.opts.timeout).await {
            Ok(output) => output,
            Err(e) => {
                tracing::warn!("the kept runner failed; replaying from the top: {e:#}");
                return Ok(None);
            }
        };
        let mut results: Vec<CellResult> = self
            .last
            .results
            .iter()
            .filter(|r| r.id != cell.id)
            .cloned()
            .collect();
        results.extend(output.results);
        let mut result = finish(&self.router, results, reads_before).await?;
        result.reads_state |= self.last.reads_state;
        if result.ok() {
            self.runner = Some(runner);
        }
        Ok(Some(result))
    }

    /// Append a cell by running only that cell. `None` means it could not be tried and
    /// nothing changed.
    async fn append_directly(&mut self, candidate: &Notebook) -> Result<Option<Applied>> {
        if self.runner.is_none() {
            return Ok(None);
        }
        let cell = candidate.cells.last().map(|c| c.id.clone());
        let Some(result) = self.run_last_cell(candidate).await? else {
            // The runner broke part-way: the scene may hold part of the cell.
            self.replay_kept_checked().await?;
            return Ok(None);
        };
        if !result.ok() {
            self.replay_kept_checked().await?;
            return Ok(Some(Applied {
                ok: false,
                cell,
                replay: result,
                replayed: false,
            }));
        }
        let before = self.notebook.print();
        write_atomic(&self.path, &candidate.print())?;
        self.push_undo(before);
        self.notebook = candidate.clone();
        self.last = result.clone();
        self.unverified += 1;
        Ok(Some(Applied {
            ok: true,
            cell,
            replay: result,
            replayed: false,
        }))
    }

    /// Check the cells that were run alone against the definition: replay the notebook from
    /// the top and compare. Afterwards the scene is the replayed one either way. A mismatch
    /// means some cell does not do the same thing every time it runs.
    pub async fn verify(&mut self) -> Result<Verified> {
        if let Some(mismatch) = self.mismatch.take() {
            return Ok(mismatch);
        }
        let checked = self.unverified;
        let had = self.last.hash.clone();
        if checked == 0 {
            return Ok(Verified {
                checked,
                consistent: true,
                replayed: had.clone(),
                had,
            });
        }
        self.replay_kept().await?;
        Ok(Verified {
            checked,
            consistent: self.last.ok() && self.last.hash == had,
            had,
            replayed: self.last.hash.clone(),
        })
    }

    /// Restore the notebook as it was before the latest kept change. `None` when there is
    /// nothing to undo.
    pub async fn undo(&mut self) -> Result<Option<&ReplayResult>> {
        let Some(text) = self.undo.pop() else {
            return Ok(None);
        };
        let notebook = Notebook::parse(&text).context("the undo entry no longer parses")?;
        write_atomic(&self.path, &text)?;
        self.notebook = notebook;
        self.replay_kept().await?;
        Ok(Some(&self.last))
    }

    fn push_undo(&mut self, text: String) {
        self.undo.push(text);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    // ---- engine access for read-only questions -------------------------------------------

    /// One pure engine call on behalf of `origin_kind` (`agent`, `chip`). Scene-changing
    /// operations are refused: the scene changes only through `apply`.
    pub async fn query(&self, fn_name: &str, args: Value, origin_kind: &str) -> Result<Value> {
        match self.is_pure(fn_name) {
            None => bail!("there is no operation `{fn_name}`"),
            Some(false) => bail!(
                "`{fn_name}` changes the scene; a query may only use pure operations \
                 (change the scene by proposing a cell)"
            ),
            Some(true) => {}
        }
        self.router
            .router()
            .handle(
                "call",
                json!({"fn": fn_name, "args": args, "origin": {"kind": origin_kind}}),
            )
            .await
            .map_err(|(code, detail)| anyhow!("engine call failed ({code}): {detail}"))
    }

    /// The engine's manifest, with availability for the current scene.
    pub async fn api(&self) -> Result<Value> {
        Ok(self.engine.describe_api(true).await?)
    }

    /// Any engine method not covered above (`sample`, `export.render`, `code.glsl`).
    pub async fn engine_request(&self, method: &str, params: Value) -> Result<Value> {
        Ok(self.engine.request(method, params).await?)
    }

    // ---- chips and direct controls (ARCHITECTURE 11) -------------------------------------

    /// The chips for the current scene: derived from the scene state and the engine's masked
    /// operation list, with no model involved.
    pub async fn chips(&self) -> Result<Vec<Chip>> {
        let api = self.api().await?;
        Ok(chips::for_scene(self.state(), &api))
    }

    /// Fire a direct chip: its code becomes a cell, like any other edit.
    pub async fn fire_chip(&mut self, chip_id: &str) -> Result<Applied> {
        let chip = self
            .chips()
            .await?
            .into_iter()
            .find(|c| c.id == chip_id)
            .ok_or_else(|| anyhow!("there is no chip `{chip_id}` for the current scene"))?;
        match (chip.kind, chip.code) {
            (ChipKind::Direct, Some(code)) => {
                self.apply(CellEdit::Append {
                    code,
                    caption: chip.label,
                })
                .await
            }
            _ => bail!("chip `{chip_id}` is not a direct chip; it needs input"),
        }
    }

    /// A slider release: one cell that sets the parameter. Consecutive commits of the same
    /// parameter replace the previous commit cell instead of piling up.
    pub async fn commit_param(&mut self, name: &str, value: f64) -> Result<Applied> {
        if !value.is_finite() {
            bail!("a parameter value must be a finite number");
        }
        if self.state()["params"].get(name).is_none() {
            bail!("there is no parameter `{name}` in this scene");
        }
        self.sync_from_disk().await?;
        let code = chips::param_set_code(name, value);
        let caption = format!("Set {name} = {value}");
        let previous = self
            .notebook
            .cells
            .last()
            .filter(|c| chips::is_param_commit(&c.code, name))
            .map(|c| c.id.clone());
        if let Some(applied) = self
            .commit_param_directly(name, value, &code, &caption, previous.as_deref())
            .await?
        {
            return Ok(applied);
        }
        let edit = match previous {
            Some(id) => CellEdit::Replace {
                id,
                code,
                caption: Some(caption),
            },
            None => CellEdit::Append { code, caption },
        };
        self.apply(edit).await
    }

    /// Commit a parameter by running only the commit cell (ARCHITECTURE 5.6).
    ///
    /// The commit cell is the last cell of the notebook, so no other cell can depend on it:
    /// running it alone in the kept runner gives the scene, revision and hash a replay from
    /// the top gives. Replacing the previous commit of the same parameter first puts the
    /// scene back to the revision before that cell. Scene objects hold parameters as
    /// symbols, so the new value changes no object; only the objects that depend on the
    /// parameter are sampled again. Returns `None` when this cannot be done and a replay is
    /// needed.
    async fn commit_param_directly(
        &mut self,
        name: &str,
        value: f64,
        code: &str,
        caption: &str,
        previous: Option<&str>,
    ) -> Result<Option<Applied>> {
        if !self.last.ok() {
            return Ok(None);
        }
        // An out-of-range value is refused by the engine; let the replay path report it.
        let param = &self.state()["params"][name];
        let in_range = match (param["min"].as_f64(), param["max"].as_f64()) {
            (Some(min), Some(max)) => (min..=max).contains(&value),
            _ => false,
        };
        if !in_range {
            return Ok(None);
        }
        // Replacing the previous commit means starting from the scene before it: the revision
        // after the nearest earlier cell that made a call (a cell that made none changed nothing).
        let base = match previous {
            None => self.last.revision,
            Some(id) => {
                let router = self.router.router();
                let earlier = self
                    .notebook
                    .cells
                    .iter()
                    .rev()
                    .skip_while(|c| c.id != id)
                    .skip(1);
                earlier
                    .map(|c| c.id.as_str())
                    .chain(std::iter::once(PREAMBLE_ID))
                    .find_map(|cell| router.revision_after(cell))
                    .unwrap_or(0)
            }
        };
        let mut candidate = self.notebook.clone();
        let cell = match previous {
            Some(id) => {
                candidate.replace(id, code, Some(caption))?;
                id.to_string()
            }
            None => candidate.append(code, caption)?,
        };
        if self.runner.is_none() {
            return Ok(None);
        }
        if previous.is_some() && self.engine.restore(base).await.is_err() {
            // The revision is no longer kept. Nothing has changed yet.
            return Ok(None);
        }
        // The commit cell runs in the kept runner like any appended cell, so the namespace
        // stays what a replay builds (the cell imports `param`).
        let result = match self.run_last_cell(&candidate).await? {
            Some(result) if result.ok() => result,
            _ => {
                // Unexpected: put everything back and let the replay path report it.
                self.replay_kept().await?;
                return Ok(None);
            }
        };
        let before = self.notebook.print();
        write_atomic(&self.path, &candidate.print())?;
        self.push_undo(before);
        self.notebook = candidate;
        self.last = result.clone();
        Ok(Some(Applied {
            ok: true,
            cell: Some(cell),
            replay: result,
            replayed: false,
        }))
    }

    /// Stop the engine. The router stops when the session is dropped.
    pub async fn close(self) -> Result<()> {
        self.engine.shutdown().await.ok();
        Ok(())
    }
}

/// Write a file through a temporary sibling and a rename, so a reader never sees half of it.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
        .to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
