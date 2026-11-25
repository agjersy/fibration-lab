//! Tool execution. Every result is capped at 16 KiB for the model; larger results are stored
//! under the session's `artifacts/` and the model receives the head plus
//! `{truncated: true, artifact: "<path>"}`.

use crate::kernel::KernelClient;
use crate::service::decks::DeckServices;
use crate::service::fs::{Page, WorkspaceFiles};
use crate::service::packages;
use crate::service::runs::RunService;
use crate::session::Session;
use crate::target::{self, Target};
use crate::tools::registry::DeckKinds;
use crate::util::{normalize_rel_path, now_rfc3339, tail_bytes, truncate_bytes};
use fibration_deck::service::DeckService;
use fibration_deck::tools::dispatch::{
    self as deck_tools, Change, Prepared, ToolState as DeckToolState,
};
use fibration_protocol::config::ProjectConfig;
use fibration_protocol::kernel::*;
use fibration_protocol::transcript::Event;
use fibration_protocol::ui::{OutputStream, SessionEvent};
use fibration_protocol::TOOL_RESULT_CAP_BYTES;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(m: impl Into<String>) -> Self {
        ToolError { message: m.into() }
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError::new(format!("{e:#}"))
    }
}

impl From<crate::kernel::KernelError> for ToolError {
    fn from(e: crate::kernel::KernelError) -> Self {
        ToolError::new(e.to_string())
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(e: serde_json::Error) -> Self {
        ToolError::new(format!("serialization: {e}"))
    }
}

impl From<crate::service::ServiceError> for ToolError {
    fn from(e: crate::service::ServiceError) -> Self {
        ToolError::new(e.message)
    }
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        ToolError::new(e.to_string())
    }
}

/// A capped tool result as shown to the model.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub ok: bool,
    pub result: Value,
    pub truncated: bool,
    /// Images to show a vision-capable model next to the result (a rendered slide, a page
    /// of a reference): `(media type, base64)`.
    pub images: Vec<(String, String)>,
    /// Slides (repo-relative documents) the model has now looked at.
    pub viewed: Vec<String>,
}

/// What a tool produced before capping.
#[derive(Debug, Clone, Default)]
pub struct Dispatched {
    pub value: Value,
    /// The tool ran but reports failure in its result (a skill script that exited non-zero).
    pub failed: bool,
    pub images: Vec<(String, String)>,
    pub viewed: Vec<String>,
}

impl From<Value> for Dispatched {
    fn from(value: Value) -> Self {
        Dispatched {
            value,
            ..Default::default()
        }
    }
}

/// Callback for live run output: `(stream, chunk)` with stream `stdout` or `stderr`.
pub type OutputSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Everything tools need.
pub struct ToolContext {
    pub repo: PathBuf,
    pub home: PathBuf,
    pub session: Session,
    pub conv_id: String,
    pub kernel: Option<KernelClient>,
    pub target: Target,
    pub project: ProjectConfig,
    pub control_dir: Option<PathBuf>,
    /// The session's run service: `run.script` starts runs through it, exactly as the UI's
    /// `run_start` does. A context built on its own gets its own service; a desktop session
    /// hands in the one its runtime owns ([`ToolContext::use_runs`]).
    pub runs: Arc<RunService>,
    owns_runs: bool,
    /// The session's workspace-files service, shared with the UI's `fs_*` commands.
    pub files: WorkspaceFiles,
    /// The session's decks. A deck's kernel starts when a deck tool first needs it.
    pub decks: Arc<DeckServices>,
    /// Per-conversation state of the deck tools, by deck slug (template cache, checker
    /// status, whether rendering is available).
    deck_state: tokio::sync::Mutex<std::collections::HashMap<String, DeckToolState>>,
    /// User-approved external data directories (the global config's target `data_dirs`):
    /// `fs.read` may follow a repo symlink into them. Empty by default.
    pub data_roots: Vec<String>,
    /// Skills visible to this session (user and project folders), discovered when the
    /// context is built.
    pub skills: wb_skills::SkillSet,
}

impl ToolContext {
    pub fn new(
        repo: &Path,
        home: &Path,
        session: Session,
        conv_id: &str,
        project: ProjectConfig,
    ) -> Self {
        let (session_for_runs, project_for_runs) = (session.clone(), project.clone());
        ToolContext {
            repo: repo.to_path_buf(),
            home: home.to_path_buf(),
            session,
            conv_id: conv_id.to_string(),
            kernel: None,
            target: Target::Local,
            project,
            control_dir: None,
            runs: RunService::standalone(&session_for_runs, Target::Local, project_for_runs),
            owns_runs: true,
            files: WorkspaceFiles::new(repo).with_artifacts(session_for_runs.artifacts_dir()),
            decks: DeckServices::new(repo, home, &session_for_runs.dir),
            deck_state: tokio::sync::Mutex::new(Default::default()),
            data_roots: vec![],
            skills: crate::skills::discover(home, repo),
        }
    }

    /// Set the kernel the tools talk to. A context with its own run service attaches the
    /// kernel to it; a session's shared run service follows the session's kernel.
    pub fn set_kernel(&mut self, kernel: Option<KernelClient>) {
        if self.owns_runs {
            self.runs.attach_kernel(kernel.clone());
        }
        self.kernel = kernel;
    }

    /// Set the execution target (and the SSH control directory) for the tools and, for a
    /// context with its own run service, for runs.
    pub fn set_target(&mut self, target: Target, control_dir: Option<PathBuf>) {
        if self.owns_runs {
            self.runs.set_target(target.clone());
            self.runs.set_control_dir(control_dir.clone());
        }
        self.target = target;
        self.control_dir = control_dir;
    }

    /// Use a session's run service instead of the context's own.
    pub fn use_runs(&mut self, runs: Arc<RunService>) {
        self.runs = runs;
        self.owns_runs = false;
    }

    /// Print-style consumers (the CLI) get the live output of every run through `sink`.
    pub fn stream_run_output(&self, sink: OutputSink) {
        let mut rx = self.runs.hub().sender().subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(SessionEvent::RunOutput { stream, chunk, .. }) => sink(
                        match stream {
                            OutputStream::Stdout => "stdout",
                            OutputStream::Stderr => "stderr",
                        },
                        &chunk,
                    ),
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }

    fn kernel(&self) -> Result<&KernelClient, ToolError> {
        self.kernel.as_ref().ok_or_else(|| {
            ToolError::new("the kernel is not connected on this target (target offline)")
        })
    }

    /// Which kinds of decks the project has (reads the deck documents; starts nothing).
    pub fn deck_kinds(&self) -> DeckKinds {
        let decks = self.decks.list();
        DeckKinds {
            structured: decks.iter().any(|d| d.mode == "structured"),
            code: decks.iter().any(|d| d.mode == "code"),
        }
    }

    /// The slug a deck tool call means: its `deck` argument, or the only deck.
    pub fn deck_slug(&self, args: &Value) -> Result<String, ToolError> {
        if let Some(s) = args
            .get("deck")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(s.to_string());
        }
        let decks = self.decks.list();
        match decks.as_slice() {
            [one] => Ok(one.slug.clone()),
            [] => Err(ToolError::new(
                "this project has no deck; the user creates one from a template",
            )),
            many => Err(ToolError::new(format!(
                "this project has several decks; pass `deck` (one of: {})",
                many.iter()
                    .map(|d| d.slug.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// `slide.place_figure`: the figure placed on the slide by
    /// [`crate::service::artifacts::place_figure`] (the same function the UI's
    /// `slide_place_figure` uses), as the arguments of the equivalent `slide.propose_update`.
    async fn place_figure_args(&self, args: &Value) -> Result<Value, ToolError> {
        let slug = self.deck_slug(args)?;
        let run_id = str_arg(args, "run_id")?;
        let artifact = str_arg(args, "artifact")?;
        let slide_id = str_arg(args, "slide_id")?;
        let svc = self.decks.open(&slug).await?;
        let placed = crate::service::artifacts::place_figure(
            &self.runs,
            &self.session,
            &self.repo,
            &svc,
            slide_id,
            run_id,
            artifact,
            args.get("placeholder").and_then(Value::as_str),
            args.get("alt").and_then(Value::as_str),
        )
        .await?;
        Ok(json!({"deck": slug, "slide_id": slide_id, "slide": placed.slide}))
    }

    /// `slide.place_equation`: the equation placed on the slide by
    /// [`crate::service::provenance::place_equation`] (the same function the UI's
    /// `slide_place_equation` uses), as the arguments of the equivalent `slide.propose_update`.
    async fn place_equation_args(&self, args: &Value) -> Result<Value, ToolError> {
        let slug = self.deck_slug(args)?;
        let path = str_arg(args, "path")?;
        let function = str_arg(args, "function")?;
        let slide_id = str_arg(args, "slide_id")?;
        let svc = self.decks.open(&slug).await?;
        let placed = crate::service::provenance::place_equation(
            self.kernel()?,
            &self.repo,
            &svc,
            slide_id,
            path,
            function,
            args.get("placeholder").and_then(Value::as_str),
        )
        .await?;
        Ok(json!({"deck": slug, "slide_id": slide_id, "slide": placed.slide}))
    }

    /// `slide.insert_result`: the paragraph with the bound number made by
    /// [`crate::service::provenance::bind_result`], as the arguments of the equivalent
    /// `slide.propose_update`.
    async fn insert_result_args(&self, args: &Value) -> Result<Value, ToolError> {
        let slug = self.deck_slug(args)?;
        let slide_id = str_arg(args, "slide_id")?;
        let svc = self.decks.open(&slug).await?;
        let placed = crate::service::provenance::bind_result(
            &self.runs,
            &self.session,
            &self.repo,
            &svc,
            slide_id,
            args.get("placeholder")
                .and_then(Value::as_str)
                .unwrap_or("body"),
            str_arg(args, "run_id")?,
            str_arg(args, "result_name")?,
            args.get("decimals")
                .and_then(Value::as_u64)
                .map(|d| d as usize),
            args.get("before").and_then(Value::as_str),
            args.get("after").and_then(Value::as_str),
            match (
                args.get("row").and_then(Value::as_u64),
                args.get("column").and_then(Value::as_u64),
            ) {
                (Some(r), Some(c)) => Some((r as usize, c as usize)),
                (None, None) => None,
                _ => {
                    return Err(ToolError::new(
                        "`row` and `column` go together: both address one table cell",
                    ))
                }
            },
        )?;
        Ok(json!({"deck": slug, "slide_id": slide_id, "slide": placed.slide}))
    }

    /// Run the first half of a deck tool: read tools finish here, edit tools come back as a
    /// [`Change`] for the loop to gate. Starts the deck's kernel when needed.
    pub async fn deck_prepare(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<(Arc<DeckService>, Prepared), ToolError> {
        if name == crate::tools::registry::PLACE_FIGURE {
            let update = self.place_figure_args(args).await?;
            return self
                .deck_prepare_tool("slide.propose_update", &update)
                .await;
        }
        if name == crate::tools::registry::INSERT_RESULT {
            let update = self.insert_result_args(args).await?;
            return self
                .deck_prepare_tool("slide.propose_update", &update)
                .await;
        }
        if name == crate::tools::registry::PLACE_EQUATION {
            let update = self.place_equation_args(args).await?;
            return self
                .deck_prepare_tool("slide.propose_update", &update)
                .await;
        }
        self.deck_prepare_tool(name, args).await
    }

    async fn deck_prepare_tool(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<(Arc<DeckService>, Prepared), ToolError> {
        let slug = self.deck_slug(args)?;
        let svc = self.decks.open(&slug).await?;
        let mut states = self.deck_state.lock().await;
        let state = states.entry(slug).or_default();
        if state.skills.is_none() {
            state.skills = Some(self.skills.clone());
        }
        // The deck tools do not know the `deck` argument.
        let mut inner = args.clone();
        if let Some(o) = inner.as_object_mut() {
            o.remove("deck");
        }
        let prepared = deck_tools::prepare(&svc.ws, state, name, &inner).await;
        Ok((svc, prepared))
    }

    /// Write an accepted deck change and tell the deck's UI.
    pub async fn deck_apply(
        &self,
        svc: &Arc<DeckService>,
        change: &Change,
    ) -> Result<Value, ToolError> {
        let result = if matches!(change, Change::ModeSwitch { .. }) {
            // Reversible: the deck service checkpoints the repository before the switch.
            svc.switch_to_code()
                .await
                .map_err(|e| ToolError::new(e.message))?
        } else {
            let mut states = self.deck_state.lock().await;
            let state = states.entry(svc.slug.clone()).or_default();
            deck_tools::apply(&svc.ws, state, change)
                .await
                .map_err(ToolError::new)?
        };
        if matches!(change, Change::ModeSwitch { .. }) {
            self.deck_state.lock().await.remove(&svc.slug);
        }
        let removed = match change {
            Change::SlideDelete { before } => vec![before.id.to_string()],
            _ => vec![],
        };
        let changed: Vec<String> = change
            .slide_ids()
            .into_iter()
            .filter(|id| !removed.contains(id))
            .collect();
        svc.notify_changed(changed, removed, "agent");
        Ok(result)
    }

    /// Run the deck checker on some slides of a deck (all when `None`) and return the
    /// findings.
    pub async fn deck_check(
        &self,
        svc: &Arc<DeckService>,
        slides: Option<&[String]>,
    ) -> Result<Vec<Value>, String> {
        let mut states = self.deck_state.lock().await;
        let state = states.entry(svc.slug.clone()).or_default();
        deck_tools::check_slides(&svc.ws, state, slides).await
    }

    /// Whether slides of a deck can be rendered for self-review (an engine is installed).
    pub async fn deck_render_available(&self, slug: &str) -> bool {
        self.deck_state
            .lock()
            .await
            .get(slug)
            .map(|s| !s.render_unavailable)
            .unwrap_or(true)
    }

    async fn deck_dispatch(&self, name: &str, args: &Value) -> Result<Dispatched, ToolError> {
        let (svc, prepared) = self.deck_prepare(name, args).await?;
        match prepared {
            Prepared::Done(out) => {
                if !out.ok {
                    return Err(ToolError::new(
                        out.result
                            .get("error")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| out.result.to_string()),
                    ));
                }
                Ok(Dispatched {
                    value: out.result,
                    failed: false,
                    images: out.images,
                    viewed: out
                        .viewed
                        .iter()
                        .map(|id| crate::service::decks::slide_rel(&svc.slug, id))
                        .collect(),
                })
            }
            Prepared::Denied { message, .. } => Err(ToolError::new(message)),
            // Direct dispatch (the CLI, tests): the change is applied as it is; the agent
            // loop gates changes itself before applying them.
            Prepared::Change(change) => Ok(self.deck_apply(&svc, &change).await?.into()),
        }
    }

    /// Execute a tool by internal id. Edit tools are built and applied directly here; the agent
    /// builds proposals separately when it needs approval first.
    pub async fn dispatch(
        &self,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<Value, ToolError> {
        self.dispatch_full(call_id, name, args)
            .await
            .map(|d| d.value)
    }

    /// [`ToolContext::dispatch`] with what a result carries besides its JSON (images).
    pub async fn dispatch_full(
        &self,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<Dispatched, ToolError> {
        if crate::tools::registry::is_deck_tool(name) {
            return self.deck_dispatch(name, args).await;
        }
        self.dispatch_analysis(call_id, name, args)
            .await
            .map(Dispatched::from)
    }

    async fn dispatch_analysis(
        &self,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<Value, ToolError> {
        match name {
            "fs.read" => self.fs_read(args),
            "skill.load" => {
                let skill = args.get("name").and_then(Value::as_str).unwrap_or("");
                let instructions = self
                    .skills
                    .load(skill)
                    .map_err(|e| ToolError::new(e.to_string()))?;
                Ok(json!({"name": skill, "instructions": instructions}))
            }
            "fs.list" => self.fs_list(args),
            "fs.search" => self.fs_search(args).await,
            "scripts.index" => {
                let globs = Some(self.project.project.scripts.clone());
                Ok(serde_json::to_value(
                    self.kernel()?.scripts_index(globs).await?,
                )?)
            }
            "lib.index" => {
                let pkg = Some(self.project.project.library.clone());
                Ok(serde_json::to_value(self.kernel()?.lib_index(pkg).await?)?)
            }
            "runs.list" => {
                let limit = args.get("limit").and_then(Value::as_u64);
                match self.kernel() {
                    Ok(k) => Ok(serde_json::to_value(k.runs_list(limit).await?)?),
                    Err(_) => {
                        let mut runs = self.session.read_runs()?;
                        runs.reverse();
                        runs.truncate(limit.unwrap_or(20) as usize);
                        Ok(json!({"runs": runs, "source": "session"}))
                    }
                }
            }
            "runs.get" => {
                let id = str_arg(args, "run_id")?;
                match self.kernel() {
                    Ok(k) => Ok(serde_json::to_value(k.run_status(id).await?)?),
                    Err(_) => match self.session.find_run(id)? {
                        Some(r) => Ok(serde_json::to_value(r)?),
                        None => Err(ToolError::new(format!(
                            "run `{id}` not found in this session"
                        ))),
                    },
                }
            }
            "out.list" => {
                let p: OutListParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.out_list(p).await?)?)
            }
            "out.read" => {
                let p: OutReadParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.out_read(p).await?)?)
            }
            "verify.list" => {
                let scope = crate::service::verification::Scope {
                    repo: &self.repo,
                    runs: &self.runs,
                    session: &self.session,
                };
                let only = args
                    .get("unverified_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let items: Vec<_> = crate::service::verification::list(&scope)
                    .into_iter()
                    .filter(|v| {
                        !only || v.state == fibration_protocol::ui::VerificationState::Unverified
                    })
                    .take(200)
                    .collect();
                Ok(json!({"items": items}))
            }
            "verify.record" => {
                let item: fibration_protocol::ui::VerifyItem =
                    serde_json::from_value(args.get("item").cloned().unwrap_or(Value::Null))
                        .map_err(bad_args)?;
                let state: fibration_protocol::ui::VerifyState =
                    serde_json::from_value(args.get("state").cloned().unwrap_or(Value::Null))
                        .map_err(bad_args)?;
                let scope = crate::service::verification::Scope {
                    repo: &self.repo,
                    runs: &self.runs,
                    session: &self.session,
                };
                let view = crate::service::verification::set(
                    &scope,
                    &item,
                    state,
                    fibration_protocol::ui::Verifier::Agent,
                    args.get("note").and_then(Value::as_str),
                )?;
                self.runs.events().emit(
                    fibration_protocol::ui::SessionEvent::VerificationChanged {
                        key: view.key.clone(),
                    },
                );
                Ok(serde_json::to_value(view)?)
            }
            "eq.extract" => {
                let p: EqExtractParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.eq_extract(p).await?)?)
            }
            "eq.check" => {
                let p: EqCheckParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.eq_check(p).await?)?)
            }
            "contract.check" => {
                let p: PathsParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(
                    self.kernel()?.contract_check(p.paths).await?,
                )?)
            }
            "contract.similar" => {
                let p: SimilarParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(
                    self.kernel()?.contract_similar(p.threshold).await?,
                )?)
            }
            "lint.run" => {
                let p: PathsParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(
                    self.kernel()?.lint_run(p.paths).await?,
                )?)
            }
            "fig.list" => self.fig_list(args),
            "artifacts.list" => {
                let kind = match args.get("kind").filter(|k| !k.is_null()) {
                    Some(k) => {
                        Some(serde_json::from_value::<ArtifactKind>(k.clone()).map_err(bad_args)?)
                    }
                    None => None,
                };
                let entries = crate::service::artifacts::list(
                    &self.runs,
                    &self.session,
                    &self.repo,
                    args.get("run_id").and_then(Value::as_str),
                    kind,
                    args.get("limit")
                        .and_then(Value::as_u64)
                        .map(|n| n as usize),
                );
                Ok(json!({"artifacts": entries}))
            }
            "target.info" => self.target_info().await,
            "fs.propose_edit" | "fs.propose_create" | "fs.propose_delete" => {
                let p = self.files.propose(call_id, name, args)?;
                self.files.apply(&p)?;
                Ok(json!({"applied": true, "path": p.path, "summary": p.summary}))
            }
            "run.script" => self.run_script(args).await,
            "exec.snippet" => {
                let code = str_arg(args, "code")?;
                let t = args.get("timeout_s").and_then(Value::as_f64);
                Ok(serde_json::to_value(
                    self.kernel()?.exec_snippet(code, t).await?,
                )?)
            }
            "pkg.add" | "pkg.remove" | "pkg.sync" => self.pkg(name, args).await,
            "shell.run" => self.shell_run(args).await,
            "git.status" => {
                let out = crate::checkpoint::git(
                    &self.repo,
                    &["status", "--porcelain=v1", "--branch"],
                    None,
                )?;
                Ok(json!({"status": out}))
            }
            "git.diff" => {
                let mut a = vec!["diff", "--no-color"];
                let p;
                if let Some(path) = args.get("path").and_then(Value::as_str) {
                    p = normalize_rel_path(path).map_err(ToolError::new)?;
                    a.push("--");
                    a.push(&p);
                }
                let out = crate::checkpoint::git(&self.repo, &a, None)?;
                Ok(json!({"diff": out}))
            }
            "git.commit" => {
                let msg = str_arg(args, "message")?;
                crate::checkpoint::git(&self.repo, &["add", "-A"], None)?;
                let out = crate::checkpoint::git(&self.repo, &["commit", "-q", "-m", msg], None)?;
                let sha =
                    crate::checkpoint::git(&self.repo, &["rev-parse", "--short", "HEAD"], None)?;
                Ok(json!({"committed": true, "commit": sha.trim(), "output": out}))
            }
            "declare_scope" | "pin_fact" => Err(ToolError::new(format!(
                "`{name}` is handled by the agent loop, not dispatch"
            ))),
            other => Err(ToolError::new(format!("unknown tool `{other}`"))),
        }
    }

    /// Cap a result for the model. The cap applies to the final serialisation, for errors as
    /// well as successes. Larger results are spilled to the session artifacts directory as
    /// line-oriented text (string fields raw, everything else pretty JSON), so `fs.read` with
    /// a line range can page through them.
    pub fn cap_result(&self, call_id: &str, result: Result<Value, ToolError>) -> ToolOutcome {
        let (ok, value) = match result {
            Ok(v) => (true, v),
            Err(e) => (false, json!({"error": e.message})),
        };
        let text = serde_json::to_string(&value).unwrap_or_default();
        if text.len() <= TOOL_RESULT_CAP_BYTES {
            return ToolOutcome {
                ok,
                result: value,
                truncated: false,
                images: vec![],
                viewed: vec![],
            };
        }
        let full = render_for_paging(&value);
        let safe_id: String = call_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = format!("{}/{}.txt", self.conv_id, safe_id);
        let stored = self.session.spill_artifact(&name, full.as_bytes());
        let total_lines = full.lines().count();
        let mut head_len = TOOL_RESULT_CAP_BYTES * 3 / 4;
        loop {
            let head = truncate_bytes(&full, head_len);
            let mut out = json!({
                "head": head,
                "truncated": true,
                "total_bytes": full.len(),
                "total_lines": total_lines,
            });
            if !ok {
                out["error"] = json!(truncate_bytes(&full, head_len.min(2000)));
            }
            match &stored {
                Ok(path) => {
                    out["artifact"] = json!(path.display().to_string());
                    out["hint"] = json!(
                        "page the full result with fs.read on the artifact path using start_line/end_line"
                    );
                }
                Err(e) => out["spill_error"] = json!(format!("could not store full result: {e}")),
            }
            let len = serde_json::to_string(&out).map(|t| t.len()).unwrap_or(0);
            if len <= TOOL_RESULT_CAP_BYTES || head_len < 256 {
                return ToolOutcome {
                    ok,
                    result: out,
                    truncated: true,
                    images: vec![],
                    viewed: vec![],
                };
            }
            // JSON escaping can inflate the head well past its raw size.
            head_len = head_len * 2 / 3;
        }
    }

    // ---- filesystem ----

    /// Directories outside the repo that reads may reach through a symlink: the target's
    /// user-approved data directories (local paths only make sense on the local target).
    fn read_roots(&self) -> Vec<PathBuf> {
        if self.target.is_remote() {
            return vec![];
        }
        self.data_roots
            .iter()
            .map(|d| crate::util::expand_tilde(d))
            .filter(|d| d.is_absolute())
            .collect()
    }

    fn fs_read(&self, args: &Value) -> Result<Value, ToolError> {
        let raw = str_arg(args, "path")?;
        let line = |key: &str| args.get(key).and_then(Value::as_u64).map(|n| n as usize);
        match self.files.read_page(
            raw,
            line("start_line"),
            line("end_line"),
            &self.read_roots(),
        )? {
            Page::Binary { size } => Ok(json!({"path": raw, "binary": true, "size": size})),
            Page::Text {
                start_line,
                end_line,
                total_lines,
                content,
            } => Ok(
                json!({"path": raw, "start_line": start_line, "end_line": end_line, "total_lines": total_lines, "content": content}),
            ),
        }
    }

    fn fs_list(&self, args: &Value) -> Result<Value, ToolError> {
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let depth = args
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .clamp(1, 6) as usize;
        Ok(json!({"path": raw, "entries": self.files.list(raw, depth)?}))
    }

    async fn fs_search(&self, args: &Value) -> Result<Value, ToolError> {
        let pattern = str_arg(args, "pattern")?;
        let max = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, 1000) as usize;
        let glob = args.get("glob").and_then(Value::as_str);
        let found = self.files.search(pattern, glob, max, false).await?;
        let matches: Vec<Value> = found
            .hits
            .iter()
            .map(|h| json!({"path": h.path, "line": h.line, "text": h.text}))
            .collect();
        let mut out = json!({"pattern": pattern, "matches": matches, "truncated": found.truncated});
        if let Some(w) = found.warning {
            out["warning"] = json!(w);
        }
        Ok(out)
    }

    fn fig_list(&self, args: &Value) -> Result<Value, ToolError> {
        let want = args.get("run_id").and_then(Value::as_str);
        let mut seen = std::collections::BTreeSet::new();
        let mut runs: Vec<_> = self.session.read_runs()?;
        runs.reverse();
        let mut figures = Vec::new();
        for r in runs {
            if !seen.insert(r.meta.run_id.clone()) {
                continue;
            }
            if let Some(w) = want {
                if r.meta.run_id != w {
                    continue;
                }
            }
            let dir = self.repo.join(&r.meta.out_dir);
            let session_dir = self.session.pulled_artifact_path(&r.meta.run_id, "");
            for base in [dir, session_dir] {
                if !base.is_dir() {
                    continue;
                }
                for e in walkdir::WalkDir::new(&base).into_iter().flatten() {
                    let p = e.path();
                    if !p.is_file() {
                        continue;
                    }
                    let rel = p.to_string_lossy().to_string();
                    if ArtifactKind::from_path(&rel) != ArtifactKind::Figure {
                        continue;
                    }
                    let sidecar = PathBuf::from(target::sync::sidecar_path(&rel));
                    let meta: Option<FigureSidecar> = std::fs::read_to_string(&sidecar)
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok());
                    figures.push(json!({
                        "run_id": r.meta.run_id,
                        "script": r.meta.script,
                        "path": p.strip_prefix(&self.repo).unwrap_or(p).to_string_lossy(),
                        "title": meta.as_ref().and_then(|m| m.title.clone()),
                        "axes": meta.as_ref().map(|m| m.axes.clone()),
                    }));
                }
            }
        }
        Ok(json!({"figures": figures}))
    }

    async fn target_info(&self) -> Result<Value, ToolError> {
        let hello = match &self.kernel {
            Some(k) => k.hello().await,
            None => None,
        };
        let connected = match &self.kernel {
            Some(k) => k.is_connected().await,
            None => false,
        };
        Ok(json!({
            "name": self.target.name(),
            "kind": if self.target.is_remote() { "ssh" } else { "local" },
            "host": self.target.host(),
            "connected": connected,
            "sandbox": hello.as_ref().map(|h| h.sandbox.clone()),
            "python": hello.as_ref().map(|h| h.python.clone()),
            "data_dirs": crate::config::effective_project(&self.project, self.target.name()).data_dirs,
            "target_data_dirs": self.target.data_dirs(),
        }))
    }

    // ---- runs ----

    /// Sync the mirror when the target is remote. Returns the tree hash.
    pub async fn sync_if_needed(&self) -> Result<Option<String>, ToolError> {
        self.runs
            .sync_if_needed()
            .await
            .map_err(|e| ToolError::new(e.message))
    }

    /// `run.script`: start the run through the session's run service (the same call the
    /// UI's Run button makes), wait for it, and report it to the model.
    async fn run_script(&self, args: &Value) -> Result<Value, ToolError> {
        let script = str_arg(args, "script")?;
        let script = normalize_rel_path(script).map_err(ToolError::new)?;
        let run_args: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let eff = crate::config::effective_project(&self.project, self.target.name());
        let timeout_s = args
            .get("timeout_s")
            .and_then(Value::as_f64)
            .unwrap_or(eff.timeout_s as f64);
        let kernel = self.runs.kernel().ok_or_else(|| {
            ToolError::new("the kernel is not connected on this target (target offline)")
        })?;
        let started = self
            .runs
            .start(
                &script,
                run_args.clone(),
                Some(timeout_s),
                Some(&self.conv_id),
            )
            .await
            .map_err(|e| ToolError::new(e.message))?;
        let run_id = started.meta.run_id.clone();
        // If this future is dropped before the run finishes (the turn was cancelled), the
        // guard cancels the run on the target.
        let cancel_guard = kernel.cancel_guard(&run_id);
        self.session.append_event(
            &self.conv_id,
            &Event::Run {
                ts: now_rfc3339(),
                run_id: run_id.clone(),
                script: script.clone(),
                args: run_args.clone(),
                target: self.target.name().to_string(),
            },
        )?;
        let rec = self
            .runs
            .wait(
                &run_id,
                std::time::Duration::from_secs_f64(timeout_s + 60.0),
            )
            .await;
        // Finished, or still running past the wait deadline (the agent may poll it): either
        // way this call no longer owns the run.
        cancel_guard.disarm();
        let Some(rec) = rec else {
            return Err(ToolError::new(format!("run `{run_id}` was lost")));
        };
        let status = if rec.completed {
            rec.meta.status
        } else {
            RunStatus::Running
        };
        let exit_code = rec.meta.exit_code;
        let figures: Vec<&str> = rec
            .artifacts
            .iter()
            .filter(|a| a.kind == ArtifactKind::Figure)
            .map(|a| a.path.as_str())
            .collect();
        let tail = |s: &str| tail_bytes(s, 6000);
        let mut result = json!({
            "run_id": run_id,
            "script": script,
            "args": run_args,
            "status": status,
            "exit_code": exit_code,
            "duration_s": rec.duration_s,
            "out_dir": rec.meta.out_dir,
            "stdout": tail(&rec.stdout),
            "stderr": tail(&rec.stderr),
            "artifacts": rec.artifacts.iter().map(|a| json!({"kind": a.kind, "path": a.path, "size": a.size})).collect::<Vec<_>>(),
            "figures": figures,
        });
        if status == RunStatus::Running {
            result["note"] =
                json!("the run did not finish before the wait deadline; use runs.get to poll it");
        } else if status != RunStatus::Done || exit_code.unwrap_or(0) != 0 {
            result["failed"] = json!(true);
            result["note"] =
                json!("the run failed; read stderr above, fix the cause, and run again");
        }
        if let Some(e) = rec.pull_error {
            result["pull_error"] = json!(e);
        }
        Ok(result)
    }

    /// `pkg.*`: the session's package service, the same code as the UI's package tasks.
    async fn pkg(&self, name: &str, args: &Value) -> Result<Value, ToolError> {
        let packages: Vec<String> = args
            .get("packages")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let req = packages::Request {
            op: packages::Op::parse(name)?,
            packages,
            repo: &self.repo,
            target: self.target.clone(),
            control_dir: self.control_dir.clone(),
            session_python: None,
        };
        let log = packages::change(&req, &self.runs, None).await?;
        Ok(json!({"ok": true, "output": tail_bytes(&log, 4000)}))
    }

    /// `shell.run` goes through the kernel's sandboxed REPL (`exec.snippet`) with a subprocess
    /// wrapper, since the kernel protocol has no dedicated shell method.
    async fn shell_run(&self, args: &Value) -> Result<Value, ToolError> {
        let command = str_arg(args, "command")?;
        let timeout = args
            .get("timeout_s")
            .and_then(Value::as_f64)
            .unwrap_or(120.0);
        let code = format!(
            "import subprocess as _sp, json as _json\n_r = _sp.run({cmd}, shell=True, capture_output=True, text=True, timeout={timeout})\nprint(_json.dumps({{'exit_code': _r.returncode, 'stdout': _r.stdout[-8000:], 'stderr': _r.stderr[-8000:]}}))\n",
            cmd = serde_json::to_string(command).unwrap_or_default(),
        );
        let r = self
            .kernel()?
            .exec_snippet(&code, Some(timeout + 5.0))
            .await?;
        if !r.ok {
            return Err(ToolError::new(format!(
                "shell wrapper failed: {}",
                tail_bytes(&r.stderr, 2000)
            )));
        }
        let last = r.stdout.lines().last().unwrap_or("");
        match serde_json::from_str::<Value>(last) {
            Ok(v) => Ok(v),
            Err(_) => Ok(json!({"stdout": r.stdout, "stderr": r.stderr})),
        }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new(format!("`{key}` (string) is required")))
}

fn bad_args(e: serde_json::Error) -> ToolError {
    ToolError::new(format!("invalid arguments: {e}"))
}

/// Line-oriented rendering of a tool result for the spilled artifact: top-level string
/// fields are written raw under a `## <key>` heading so their own lines can be paged.
fn render_for_paging(v: &Value) -> String {
    fn one(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        }
    }
    match v {
        Value::Object(m) => {
            let mut out = String::new();
            for (k, val) in m {
                out.push_str(&format!("## {k}\n{}\n", one(val)));
            }
            out
        }
        other => one(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        crate::checkpoint::git(repo.path(), &["init", "-q"], None).unwrap();
        std::fs::create_dir_all(repo.path().join("scripts")).unwrap();
        std::fs::write(
            repo.path().join("scripts/a.py"),
            "import argparse\n\ndef main():\n    pass\n",
        )
        .unwrap();
        let session = Session::create(home.path(), repo.path(), "local", "p/m").unwrap();
        let c = ToolContext::new(
            repo.path(),
            home.path(),
            session,
            "c1",
            ProjectConfig::default(),
        );
        (home, repo, c)
    }

    #[tokio::test]
    async fn fs_tools_and_capping() {
        let (_h, _r, c) = ctx();
        let r = c
            .dispatch(
                "1",
                "fs.read",
                &json!({"path":"scripts/a.py","start_line":3}),
            )
            .await
            .unwrap();
        assert_eq!(r["total_lines"], 4);
        assert!(r["content"]
            .as_str()
            .unwrap()
            .starts_with("    3| def main"));
        // The repo's `.env` (API keys) is not readable through the file tools.
        std::fs::write(c.repo.join(".env"), "SOME_API_KEY=sk-in-repo\n").unwrap();
        for path in [".env", "./.env", ".ENV"] {
            let e = c
                .dispatch("e", "fs.read", &json!({ "path": path }))
                .await
                .unwrap_err();
            assert!(e.message.contains("API keys"), "{}", e.message);
        }
        std::fs::remove_file(c.repo.join(".env")).unwrap();
        let l = c.dispatch("2", "fs.list", &json!({})).await.unwrap();
        let entries = l["entries"].as_array().unwrap();
        assert!(entries.iter().any(|e| e == "scripts/a.py"));
        assert!(!entries
            .iter()
            .any(|e| e.as_str().unwrap().starts_with(".git")));
        let s = c
            .dispatch("3", "fs.search", &json!({"pattern":"def main"}))
            .await
            .unwrap();
        assert_eq!(s["matches"][0]["path"], "scripts/a.py");
        assert_eq!(s["matches"][0]["line"], 3);
        assert!(c
            .dispatch("4", "fs.read", &json!({"path":"../etc/passwd"}))
            .await
            .is_err());
        assert!(c
            .dispatch("5", "scripts.index", &json!({}))
            .await
            .unwrap_err()
            .message
            .contains("not connected"));

        // A6: a long multi-line string must be pageable by line from the artifact.
        let long: String = (1..=3000)
            .map(|i| format!("row {i} \"quoted\"\n"))
            .collect();
        let big = json!({"stdout": long, "n": 3000});
        let o = c.cap_result("big", Ok(big));
        assert!(o.truncated);
        assert_eq!(o.result["truncated"], true);
        assert!(
            serde_json::to_string(&o.result).unwrap().len() <= TOOL_RESULT_CAP_BYTES,
            "the cap applies to the final serialisation"
        );
        let art = o.result["artifact"].as_str().unwrap().to_string();
        assert!(Path::new(&art).is_file());
        assert!(o.result["total_lines"].as_u64().unwrap() > 3000);
        // The artifact is readable through fs.read by absolute path, line by line, well
        // past the point where the head was cut.
        let page = c
            .dispatch(
                "6",
                "fs.read",
                &json!({"path": art, "start_line": 2900, "end_line": 2901}),
            )
            .await
            .unwrap();
        let content = page["content"].as_str().unwrap();
        assert!(content.contains("row 289"), "{content}");
        assert_eq!(content.lines().count(), 2);
        // Error results are capped too.
        let huge_err = c.cap_result("err", Err(ToolError::new("e".repeat(100_000))));
        assert!(!huge_err.ok && huge_err.truncated);
        assert!(serde_json::to_string(&huge_err.result).unwrap().len() <= TOOL_RESULT_CAP_BYTES);
        assert!(huge_err.result["error"]
            .as_str()
            .unwrap()
            .starts_with("## error"));
        let small = c.cap_result("s", Ok(json!({"a":1})));
        assert!(!small.truncated);
        let err = c.cap_result("e", Err(ToolError::new("nope")));
        assert!(!err.ok);
        assert_eq!(err.result["error"], "nope");
    }

    #[tokio::test]
    async fn edit_tools_apply_and_git_tools_work() {
        let (_h, r, c) = ctx();
        c.dispatch(
            "1",
            "fs.propose_create",
            &json!({"path":"analysis/new.py","content":"x = 1\n"}),
        )
        .await
        .unwrap();
        assert!(r.path().join("analysis/new.py").is_file());
        c.dispatch(
            "2",
            "fs.propose_edit",
            &json!({"path":"analysis/new.py","edits":[{"old":"x = 1","new":"x = 2"}]}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(r.path().join("analysis/new.py")).unwrap(),
            "x = 2\n"
        );
        let st = c.dispatch("3", "git.status", &json!({})).await.unwrap();
        assert!(st["status"].as_str().unwrap().contains("analysis/"));
        let cm = c
            .dispatch("4", "git.commit", &json!({"message":"m"}))
            .await
            .unwrap();
        assert_eq!(cm["committed"], true);
        c.dispatch("5", "fs.propose_delete", &json!({"path":"analysis/new.py"}))
            .await
            .unwrap();
        assert!(!r.path().join("analysis/new.py").exists());
        let d = c.dispatch("6", "git.diff", &json!({})).await.unwrap();
        assert!(d["diff"].as_str().unwrap().contains("x = 2"));
    }

    /// A5: an invalid regex is an error, not "no matches".
    #[tokio::test]
    async fn search_reports_ripgrep_errors() {
        let (_h, _r, c) = ctx();
        let err = c
            .dispatch("1", "fs.search", &json!({"pattern": "def main("}))
            .await
            .unwrap_err();
        assert!(err.message.contains("ripgrep failed"), "{}", err.message);
        assert!(err.message.contains("regex"), "{}", err.message);
        let none = c
            .dispatch(
                "2",
                "fs.search",
                &json!({"pattern": "no_such_symbol_anywhere"}),
            )
            .await
            .unwrap();
        assert_eq!(none["matches"].as_array().unwrap().len(), 0);
        assert_eq!(none["truncated"], false);
        // Output is bounded by max_results.
        let many: String = (0..500).map(|i| format!("needle {i}\n")).collect();
        std::fs::write(c.repo.join("many.txt"), many).unwrap();
        let some = c
            .dispatch(
                "3",
                "fs.search",
                &json!({"pattern": "needle", "max_results": 7}),
            )
            .await
            .unwrap();
        assert_eq!(some["matches"].as_array().unwrap().len(), 7);
        assert_eq!(some["truncated"], true);
    }

    /// G7: symlinks cannot take read, list, or write outside the repo.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_do_not_escape_the_repo() {
        use std::os::unix::fs::symlink;
        let (_h, r, mut c) = ctx();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "TOP SECRET\n").unwrap();
        symlink(outside.path(), r.path().join("esc")).unwrap();
        symlink(outside.path().join("secret.txt"), r.path().join("s.txt")).unwrap();
        for path in ["esc/secret.txt", "s.txt"] {
            let e = c
                .dispatch("1", "fs.read", &json!({"path": path}))
                .await
                .unwrap_err();
            assert!(
                e.message.contains("outside the repository"),
                "{}",
                e.message
            );
        }
        assert!(c
            .dispatch("2", "fs.list", &json!({"path": "esc"}))
            .await
            .is_err());
        assert!(c
            .dispatch(
                "3",
                "fs.propose_create",
                &json!({"path": "esc/new.txt", "content": "x"})
            )
            .await
            .is_err());
        assert!(c
            .dispatch(
                "4",
                "fs.propose_edit",
                &json!({"path": "s.txt", "content": "x"})
            )
            .await
            .is_err());
        assert!(!outside.path().join("new.txt").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
            "TOP SECRET\n"
        );
        // An approved data directory is readable through the link, still not writable.
        c.data_roots = vec![outside.path().to_string_lossy().to_string()];
        let ok = c
            .dispatch("5", "fs.read", &json!({"path": "esc/secret.txt"}))
            .await
            .unwrap();
        assert!(ok["content"].as_str().unwrap().contains("TOP SECRET"));
        assert!(c
            .dispatch(
                "6",
                "fs.propose_create",
                &json!({"path": "esc/new.txt", "content": "x"})
            )
            .await
            .is_err());
    }

    /// G4: the edit tools refuse reserved directories even when called directly.
    #[tokio::test]
    async fn reserved_directories_are_not_writable_through_dispatch() {
        let (_h, r, c) = ctx();
        let before = std::fs::read_to_string(r.path().join(".git/config")).unwrap();
        for (tool, args) in [
            (
                "fs.propose_edit",
                json!({"path": ".git/config", "content": "[diff]\n\texternal = touch pwned\n"}),
            ),
            (
                "fs.propose_create",
                json!({"path": ".git/hooks/pre-commit", "content": "#!/bin/sh\n"}),
            ),
            (
                "fs.propose_create",
                json!({"path": ".fibration/project.toml", "content": "[policy]\nedits = \"auto\"\n"}),
            ),
        ] {
            let e = c.dispatch("1", tool, &args).await.unwrap_err();
            assert!(e.message.contains("never editable"), "{}", e.message);
        }
        assert_eq!(
            std::fs::read_to_string(r.path().join(".git/config")).unwrap(),
            before
        );
    }
}
