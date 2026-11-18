//! `SessionRuntime`: one open session. Owns the repo, target, project config with session policy
//! overrides, the lazily started kernel (with `kernel_state` events and reconnect), the file
//! watcher, the per-session event broadcast, conversations, runs, figures, equations, package
//! tasks, output previews, filesystem operations, and diagnostics.

use super::agent_runner::ConversationRuntime;
use super::error::{ServiceError, ServiceResult};
use super::events::{EventHub, Subscription};
use super::figures::{self, ScannedRun};
use super::fs;
use super::tasks::{TaskLogger, TaskRegistry};
use crate::config;
use crate::kernel::{
    resolve_tooling_python_in, KernelArgs, KernelClient, KernelTransport, LocalStdio, SshStdio,
};
use crate::session::Session;
use crate::target::{self, Target};
use crate::util::{normalize_rel_path, now_rfc3339, resolve_in_repo};
use analyst_protocol::config::{GlobalConfig, PolicyConfig, ProjectConfig, TargetKind};
use analyst_protocol::evals::Mode;
use analyst_protocol::kernel::*;
use analyst_protocol::ui::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Last N bytes of stdout/stderr carried in a `RunView`.
pub const TAIL_BYTES: usize = 4096;
/// Bytes of each stream kept in memory per run (the full text is in the run's log files).
pub const RUN_MEMORY_BYTES: usize = 64 * 1024;
/// `run_get` returns at most this much of each stream (the tail), with a note when cut.
pub const RUN_GET_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Finished runs kept in memory; older ones are served from the session store.
pub const MAX_FINISHED_RUNS: usize = 200;
/// Watcher debounce.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// Minimum time between mirror syncs triggered by index, check, and equation requests (R4).
pub const SYNC_DEBOUNCE: Duration = Duration::from_secs(2);
/// A failed background kernel start is not retried by passive requests for this long.
const START_BACKOFF: Duration = Duration::from_secs(10);

/// State of the local tooling environment (the interpreter that hosts `analyst_kernel`). A
/// packaged app prepares it in the background on first start (B12); kernels wait for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolingState {
    Ready,
    Preparing,
    /// Preparation failed; the message names what is missing (for example `uv`).
    Failed(String),
}

/// Shared by the workbench and every session runtime.
pub struct Tooling {
    tx: tokio::sync::watch::Sender<ToolingState>,
}

impl Tooling {
    pub fn new(state: ToolingState) -> Arc<Tooling> {
        let (tx, _) = tokio::sync::watch::channel(state);
        Arc::new(Tooling { tx })
    }

    pub fn ready() -> Arc<Tooling> {
        Tooling::new(ToolingState::Ready)
    }

    pub fn state(&self) -> ToolingState {
        self.tx.borrow().clone()
    }

    pub fn set(&self, state: ToolingState) {
        self.tx.send_replace(state);
    }

    /// Wait while the environment is being prepared. `Err` carries the failure message.
    pub async fn wait(&self) -> Result<(), String> {
        let mut rx = self.tx.subscribe();
        loop {
            let cur = rx.borrow_and_update().clone();
            match cur {
                ToolingState::Ready => return Ok(()),
                ToolingState::Failed(m) => return Err(m),
                ToolingState::Preparing => {
                    if rx.changed().await.is_err() {
                        return Err("the tooling setup was abandoned".into());
                    }
                }
            }
        }
    }
}

impl std::fmt::Debug for Tooling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tooling({:?})", self.state())
    }
}

/// Per-session options the workbench passes down.
#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    /// Interpreter hosting `analyst_kernel` (local target). `None` resolves it when a kernel
    /// starts (`resolve_tooling_python_in`), so a tooling environment bootstrapped after the
    /// session was opened is picked up.
    pub tooling_python: Option<String>,
    /// The local tooling environment's state; kernels wait while it is being prepared.
    pub tooling: Arc<Tooling>,
    /// Environment variables removed from kernels (provider API keys, U11).
    pub env_remove: Vec<String>,
    /// Override of the local kernel launcher (tests inject a fake kernel).
    pub kernel_launcher: Option<Vec<String>>,
    /// `--sandbox` override for the kernel.
    pub sandbox: Option<String>,
    /// Session interpreter override (default `<repo>/.venv/bin/python`, else `python3`).
    pub session_python: Option<String>,
    /// Start the kernel in the background when the session opens.
    pub autostart_kernel: bool,
    /// `$ANALYST_HOME/ssh` for multiplexed SSH connections.
    pub control_dir: Option<PathBuf>,
    /// Directory containing `analyst_mpl` (terminal figure capture).
    pub mpl_dir: Option<PathBuf>,
    /// The kernel source bundled with a packaged app (used to build a wheel for remote
    /// targets when no development wheel exists).
    pub kernel_source: Option<PathBuf>,
}

/// `meta.json` in the session directory: workbench-level session state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionMeta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub last_opened_at: String,
    #[serde(default)]
    pub active_conversation: Option<String>,
    #[serde(default)]
    pub conversations: Vec<ConversationMeta>,
    /// Session policy override (not written to project.toml).
    #[serde(default)]
    pub policy_override: Option<PolicyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    pub created_at: String,
}

pub fn meta_path(session_dir: &Path) -> PathBuf {
    session_dir.join("meta.json")
}

pub fn load_meta(session_dir: &Path) -> SessionMeta {
    std::fs::read_to_string(meta_path(session_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_meta(session_dir: &Path, meta: &SessionMeta) -> ServiceResult<()> {
    let text = serde_json::to_string_pretty(meta)?;
    std::fs::write(meta_path(session_dir), text)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSource {
    Kernel,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub meta: RunMeta,
    pub target: String,
    /// The last [`RUN_MEMORY_BYTES`] of each stream; the full text lives in the log files.
    pub stdout: String,
    pub stderr: String,
    pub artifacts: Vec<Artifact>,
    pub source: RunSource,
    pub finished: bool,
    /// The conversation whose agent turn started the run (IPC.md section 7).
    pub conversation: Option<String>,
}

impl RunRecord {
    pub fn view(&self) -> RunView {
        RunView {
            conversation: self.conversation.clone(),
            meta: self.meta.clone(),
            target: self.target.clone(),
            artifacts: self.artifacts.clone(),
            stdout_tail: tail(&self.stdout, TAIL_BYTES),
            stderr_tail: tail(&self.stderr, TAIL_BYTES),
        }
    }
}

pub fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Append to a bounded in-memory tail.
fn push_bounded(buf: &mut String, chunk: &str) {
    buf.push_str(chunk);
    if buf.len() > RUN_MEMORY_BYTES * 2 {
        let mut start = buf.len() - RUN_MEMORY_BYTES;
        while start < buf.len() && !buf.is_char_boundary(start) {
            start += 1;
        }
        buf.drain(..start);
    }
}

/// The last `max` bytes of a file as text, with a note in front when the start was cut.
/// `None` when the file does not exist.
fn read_log_tail(path: &Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let skip = len.saturating_sub(max);
    if skip > 0 {
        f.seek(SeekFrom::Start(skip)).ok()?;
    }
    let mut bytes = Vec::with_capacity((len - skip) as usize);
    f.take(max).read_to_end(&mut bytes).ok()?;
    let mut start = 0;
    if skip > 0 {
        // Do not start in the middle of a UTF-8 sequence.
        while start < bytes.len() && start < 4 && (bytes[start] & 0xC0) == 0x80 {
            start += 1;
        }
    }
    let text = String::from_utf8_lossy(&bytes[start..]);
    Some(if skip > 0 {
        format!("[... output truncated: the first {skip} bytes are not shown ...]\n{text}")
    } else {
        text.to_string()
    })
}

fn append_log(path: &Path, chunk: &str) {
    use std::io::Write;
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(chunk.as_bytes());
    }
}

struct KernelSlot {
    client: tokio::sync::RwLock<Option<KernelClient>>,
    state: Mutex<KernelState>,
    starting: tokio::sync::Mutex<()>,
    /// Bumped whenever the client is replaced; a forwarder of an older generation stops.
    generation: AtomicU64,
    forwarder: Mutex<Option<JoinHandle<()>>>,
    last_failure: Mutex<Option<std::time::Instant>>,
    /// How the kernel process started last is kept off the network (`bwrap`,
    /// `sandbox-exec`, `none`); `None` before the first start.
    net_isolation: Mutex<Option<String>>,
}

/// A proposal waiting for `proposal_respond`.
pub struct PendingProposal {
    pub conversation: String,
    pub tx: tokio::sync::oneshot::Sender<ProposalDecision>,
    /// The card as it was sent, so `session_info` can list it again after a renderer reload.
    pub view: ProposalView,
    /// Order of arrival.
    pub seq: u64,
}

/// The per-directory file watcher and the directories it covers.
struct WatchState {
    watcher: notify::RecommendedWatcher,
    watched: std::collections::HashSet<PathBuf>,
}

pub struct SessionRuntime {
    pub id: String,
    pub repo: PathBuf,
    pub home: PathBuf,
    pub dir: PathBuf,
    pub opts: RuntimeOptions,
    pub global: RwLock<GlobalConfig>,
    session: Mutex<Session>,
    meta: Mutex<SessionMeta>,
    target: RwLock<Target>,
    project: RwLock<ProjectConfig>,
    kernel: KernelSlot,
    events: EventHub,
    runs: Mutex<HashMap<String, RunRecord>>,
    /// run id -> conversation, from the transcripts on disk plus the turns of this process.
    run_conversations: Mutex<HashMap<String, String>>,
    run_conversations_loaded: AtomicBool,
    run_registration: tokio::sync::Mutex<()>,
    index_cache: tokio::sync::RwLock<Option<IndexResult>>,
    conversations: Mutex<HashMap<String, Arc<ConversationRuntime>>>,
    pub(super) pending: Mutex<HashMap<ProposalId, PendingProposal>>,
    pending_seq: AtomicU64,
    pub tasks: Arc<TaskRegistry>,
    watcher: Mutex<Option<WatchState>>,
    /// Why the file watcher is not (fully) working, if it is not (U2).
    watcher_error: Mutex<Option<String>>,
    /// Whether the remote target was prepared (bootstrap, sync, session env) since it was set.
    remote_prepared: tokio::sync::Mutex<bool>,
    /// Local files changed since the last mirror sync.
    mirror_dirty: AtomicBool,
    last_mirror_sync: Mutex<Option<std::time::Instant>>,
    background: Mutex<Vec<JoinHandle<()>>>,
    closed: AtomicBool,
    last_sync_hash: tokio::sync::Mutex<Option<String>>,
    /// Test hook: a model client used for every conversation instead of the configured one.
    model_override: Mutex<Option<Arc<dyn wb_providers::ModelClient>>>,
    /// `Weak` self reference so background tasks and approvers can reach the runtime.
    this: std::sync::Weak<SessionRuntime>,
}

impl SessionRuntime {
    /// Open a stored session. Starts the watcher; the kernel starts in the background when
    /// `opts.autostart_kernel` is set.
    pub fn open(
        session: Session,
        global: GlobalConfig,
        opts: RuntimeOptions,
        tasks: Arc<TaskRegistry>,
    ) -> ServiceResult<Arc<SessionRuntime>> {
        let repo = session.repo();
        if !repo.is_dir() {
            return Err(ServiceError::not_found(format!(
                "repo `{}` does not exist",
                repo.display()
            )));
        }
        let project = config::load_project(&repo)?;
        let target_cfg = config::find_target(&global, &session.record.target)?;
        let target = Target::from_config(&target_cfg)?;
        let dir = session.dir.clone();
        let mut meta = load_meta(&dir);
        meta.last_opened_at = now_rfc3339();
        if meta.conversations.is_empty() {
            let existing = session.conversations().unwrap_or_default();
            for id in existing {
                meta.conversations.push(ConversationMeta {
                    id: id.clone(),
                    title: id.clone(),
                    model: None,
                    mode: Mode::Build,
                    created_at: now_rfc3339(),
                });
            }
        }
        if meta.conversations.is_empty() {
            meta.conversations.push(ConversationMeta {
                id: crate::util::stamped_id(),
                title: "Conversation 1".into(),
                model: None,
                mode: Mode::Build,
                created_at: now_rfc3339(),
            });
        }
        let active_ok = meta
            .active_conversation
            .as_ref()
            .map(|a| meta.conversations.iter().any(|c| &c.id == a))
            .unwrap_or(false);
        if !active_ok {
            meta.active_conversation = meta.conversations.last().map(|c| c.id.clone());
        }
        save_meta(&dir, &meta)?;
        let events = EventHub::new();
        let rt = Arc::new_cyclic(|weak| SessionRuntime {
            id: session.id().to_string(),
            repo: repo.clone(),
            home: session.home.clone(),
            dir,
            opts,
            global: RwLock::new(global),
            session: Mutex::new(session),
            meta: Mutex::new(meta),
            target: RwLock::new(target),
            project: RwLock::new(project),
            kernel: KernelSlot {
                client: tokio::sync::RwLock::new(None),
                state: Mutex::new(KernelState::Stopped),
                starting: tokio::sync::Mutex::new(()),
                generation: AtomicU64::new(0),
                forwarder: Mutex::new(None),
                last_failure: Mutex::new(None),
                net_isolation: Mutex::new(None),
            },
            events,
            runs: Mutex::new(HashMap::new()),
            run_conversations: Mutex::new(HashMap::new()),
            run_conversations_loaded: AtomicBool::new(false),
            run_registration: tokio::sync::Mutex::new(()),
            index_cache: tokio::sync::RwLock::new(None),
            conversations: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            pending_seq: AtomicU64::new(0),
            tasks,
            watcher: Mutex::new(None),
            watcher_error: Mutex::new(None),
            remote_prepared: tokio::sync::Mutex::new(false),
            mirror_dirty: AtomicBool::new(true),
            last_mirror_sync: Mutex::new(None),
            background: Mutex::new(vec![]),
            closed: AtomicBool::new(false),
            last_sync_hash: tokio::sync::Mutex::new(None),
            model_override: Mutex::new(None),
            this: weak.clone(),
        });
        rt.start_watcher();
        if rt.opts.autostart_kernel {
            let r = rt.clone();
            rt.spawn(async move {
                let _ = r.ensure_kernel().await;
            });
        }
        Ok(rt)
    }

    /// Use `client` for every conversation (tests with `ScriptedModelClient`).
    pub fn set_model_override(&self, client: Option<Arc<dyn wb_providers::ModelClient>>) {
        if let Ok(mut m) = self.model_override.lock() {
            *m = client;
        }
    }

    pub fn model_override(&self) -> Option<Arc<dyn wb_providers::ModelClient>> {
        self.model_override.lock().ok().and_then(|m| m.clone())
    }

    pub(super) fn arc(&self) -> Option<Arc<SessionRuntime>> {
        self.this.upgrade()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let h = tokio::spawn(fut);
        if let Ok(mut b) = self.background.lock() {
            b.retain(|h| !h.is_finished());
            b.push(h);
        }
    }

    // ---- events ----

    /// The control broadcast, for producers (task loggers, agent callbacks).
    pub fn events(&self) -> broadcast::Sender<SessionEvent> {
        self.events.sender()
    }

    pub fn hub(&self) -> EventHub {
        self.events.clone()
    }

    /// A new subscriber. A known watcher failure is reported to it right away, since the
    /// failure usually happened before anyone was listening.
    pub fn subscribe(&self) -> Subscription {
        let sub = self.events.subscribe();
        let err = self.watcher_error.lock().ok().and_then(|e| e.clone());
        if let Some(message) = err {
            self.emit(SessionEvent::TaskDone {
                task_id: format!("task-watcher-{}", self.id),
                ok: false,
                message: Some(message),
            });
        }
        sub
    }

    pub fn emit(&self, e: SessionEvent) {
        self.events.emit(e);
    }

    // ---- state accessors ----

    pub fn session_snapshot(&self) -> Session {
        self.session
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn target(&self) -> Target {
        self.target
            .read()
            .map(|t| t.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn project(&self) -> ProjectConfig {
        self.project
            .read()
            .map(|p| p.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn global(&self) -> GlobalConfig {
        self.global
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn meta(&self) -> SessionMeta {
        self.meta
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    fn update_meta<R>(&self, f: impl FnOnce(&mut SessionMeta) -> R) -> ServiceResult<R> {
        let mut m = self
            .meta
            .lock()
            .map_err(|_| ServiceError::internal("session meta poisoned"))?;
        let r = f(&mut m);
        save_meta(&self.dir, &m)?;
        Ok(r)
    }

    /// Project policy with the session override applied.
    pub fn effective_policy(&self) -> PolicyConfig {
        match self.meta().policy_override {
            Some(p) => p,
            None => self.project().policy,
        }
    }

    /// Project config with the effective policy.
    pub fn effective_project(&self) -> ProjectConfig {
        let mut p = self.project();
        p.policy = self.effective_policy();
        p
    }

    pub fn out_dir(&self) -> String {
        config::effective_project(&self.project(), self.target().name()).out_dir
    }

    pub fn session_python(&self) -> String {
        if let Some(p) = &self.opts.session_python {
            return p.clone();
        }
        let venv = self.repo.join(".venv/bin/python");
        if venv.is_file() {
            venv.to_string_lossy().to_string()
        } else {
            "python3".to_string()
        }
    }

    pub fn kernel_state(&self) -> KernelState {
        self.kernel
            .state
            .lock()
            .map(|s| *s)
            .unwrap_or(KernelState::Error)
    }

    fn set_kernel_state(&self, state: KernelState, message: Option<String>) {
        if let Ok(mut s) = self.kernel.state.lock() {
            *s = state;
        }
        self.emit(SessionEvent::KernelState {
            state,
            message,
            network_isolation: self.kernel_network_isolation(),
        });
    }

    /// How the kernel process is kept off the network, once one has been started.
    pub fn kernel_network_isolation(&self) -> Option<String> {
        self.kernel
            .net_isolation
            .lock()
            .ok()
            .and_then(|s| s.clone())
    }

    pub fn name(&self) -> String {
        self.meta().name.unwrap_or_else(|| {
            self.repo
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| self.id.clone())
        })
    }

    pub fn summary(&self) -> SessionSummary {
        let s = self.session_snapshot();
        let meta = self.meta();
        SessionSummary {
            id: self.id.clone(),
            name: self.name(),
            repo_abs: self.repo.to_string_lossy().to_string(),
            target: s.record.target.clone(),
            model: Some(s.record.model.clone()).filter(|m| !m.is_empty() && m != "-"),
            created_at: s.record.created_at.clone(),
            last_opened_at: meta.last_opened_at.clone(),
            open: !self.is_closed(),
        }
    }

    pub async fn info(&self) -> SessionInfo {
        let summary = self.summary();
        let meta = self.meta();
        let hello = match self.kernel_client().await {
            Some(k) => k.hello().await,
            None => None,
        };
        let session = self.session_snapshot();
        let conversations = meta
            .conversations
            .iter()
            .map(|c| {
                let turns = session
                    .read_transcript(&c.id)
                    .map(|evs| {
                        evs.iter()
                            .filter(|e| {
                                matches!(e, analyst_protocol::transcript::Event::User { .. })
                            })
                            .count() as u32
                    })
                    .unwrap_or(0);
                ConversationSummary {
                    id: c.id.clone(),
                    title: c.title.clone(),
                    model: c.model.clone().or_else(|| summary.model.clone()),
                    mode: c.mode,
                    created_at: c.created_at.clone(),
                    turns,
                    busy: self.conversation(&c.id).map(|r| r.busy()).unwrap_or(false),
                }
            })
            .collect();
        let target = self.target();
        SessionInfo {
            pending_proposals: self.pending_proposals(),
            policy: PolicyView::from(&self.effective_policy()),
            kernel: self.kernel_state(),
            kernel_network_isolation: self.kernel_network_isolation(),
            sandbox: hello.as_ref().map(|h| h.sandbox.clone()),
            python_abs: Some(match &hello {
                Some(h) => h.python.clone(),
                None => self.session_python(),
            }),
            conversations,
            active_conversation: meta.active_conversation.clone().unwrap_or_default(),
            target_kind: if target.is_remote() {
                TargetKind::Ssh
            } else {
                TargetKind::Local
            },
            target_host: target.host().map(|h| h.to_string()),
            summary,
        }
    }

    /// The proposals still waiting for `proposal_respond`, oldest first.
    pub fn pending_proposals(&self) -> Vec<ProposalView> {
        let Ok(p) = self.pending.lock() else {
            return vec![];
        };
        let mut v: Vec<(u64, ProposalView)> = p.values().map(|x| (x.seq, x.view.clone())).collect();
        v.sort_by_key(|x| x.0);
        v.into_iter().map(|x| x.1).collect()
    }

    /// Register a proposal that waits for the user. The id is made unique within the session.
    pub(super) fn add_pending(
        &self,
        conversation: &str,
        mut view: ProposalView,
    ) -> (
        ProposalView,
        tokio::sync::oneshot::Receiver<ProposalDecision>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let seq = self.pending_seq.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut p) = self.pending.lock() {
            if view.id.is_empty() || p.contains_key(&view.id) {
                view.id = format!("{}-{}", view.id, crate::util::short_rand());
            }
            p.insert(
                view.id.clone(),
                PendingProposal {
                    conversation: conversation.to_string(),
                    tx,
                    view: view.clone(),
                    seq,
                },
            );
        }
        (view, rx)
    }

    pub async fn emit_session_changed(&self) {
        let info = self.info().await;
        self.emit(SessionEvent::SessionChanged { info });
    }

    pub fn rename(&self, name: &str) -> ServiceResult<SessionSummary> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ServiceError::invalid("name must not be empty"));
        }
        self.update_meta(|m| m.name = Some(name.to_string()))?;
        Ok(self.summary())
    }

    pub fn set_policy(&self, patch: &PolicyPatch) -> ServiceResult<PolicyView> {
        let mut cfg = self.effective_policy();
        patch.apply(&mut cfg);
        // Validate protected globs early.
        crate::tools::gate::Policy::new(&self.repo, cfg.clone(), Mode::Build)
            .map_err(|e| ServiceError::invalid(e.to_string()))?;
        self.update_meta(|m| m.policy_override = Some(cfg.clone()))?;
        Ok(PolicyView::from(&cfg))
    }

    pub fn layout_get(&self) -> ServiceResult<Option<String>> {
        match std::fs::read_to_string(self.dir.join("layout.json")) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn layout_save(&self, layout: &str) -> ServiceResult<()> {
        std::fs::write(self.dir.join("layout.json"), layout)?;
        Ok(())
    }

    // ---- kernel ----

    /// The interpreter hosting `analyst_kernel` for the local target, resolved now.
    pub fn tooling_python(&self) -> String {
        self.opts
            .tooling_python
            .clone()
            .unwrap_or_else(|| resolve_tooling_python_in(&self.global(), &self.home))
    }

    /// `remote_no_network`: wrap a remote kernel in `bwrap --unshare-net` (the host's probe
    /// said it works). The local kernel is always spawned through the no-network wrapper.
    fn transport(&self, remote_no_network: bool) -> Box<dyn KernelTransport> {
        let project = self.project();
        let target = self.target();
        let eff = config::effective_project(&project, target.name());
        match &target {
            Target::Local => {
                let runs = self.home.join("runs");
                let _ = std::fs::create_dir_all(&runs);
                let args = KernelArgs {
                    python: self.session_python(),
                    root: self.repo.to_string_lossy().to_string(),
                    runs_dir: runs.to_string_lossy().to_string(),
                    out_dir: Some(eff.out_dir),
                    sandbox: Some(self.opts.sandbox.clone().unwrap_or_else(|| "auto".into())),
                    data_dirs: vec![],
                };
                let t = match &self.opts.kernel_launcher {
                    Some(l) => LocalStdio::with_launcher(l.clone(), args),
                    None => LocalStdio::new(self.tooling_python(), args),
                };
                // The kernel and the scripts it runs never see provider API keys (U11).
                Box::new(t.without_env(self.opts.env_remove.clone()))
            }
            Target::Ssh { host, sandbox, .. } => {
                let mirror = target.mirror_dir(&self.repo).unwrap_or_default();
                Box::new(SshStdio {
                    host: host.clone(),
                    control_dir: self.opts.control_dir.clone(),
                    remote_launcher: vec![
                        "~/.analyst/tooling/.venv/bin/python".into(),
                        "-m".into(),
                        "analyst_kernel".into(),
                    ],
                    args: KernelArgs {
                        python: format!("{mirror}/.venv/bin/python"),
                        root: mirror,
                        runs_dir: "~/.analyst/runs".into(),
                        out_dir: Some(eff.out_dir),
                        sandbox: Some(self.opts.sandbox.clone().unwrap_or_else(|| sandbox.clone())),
                        data_dirs: target.data_dirs().to_vec(),
                    },
                    no_network: remote_no_network,
                })
            }
        }
    }

    /// The connected kernel client, if any.
    pub async fn kernel_client(&self) -> Option<KernelClient> {
        let guard = self.kernel.client.read().await;
        match guard.as_ref() {
            Some(k) if k.is_connected().await => Some(k.clone()),
            _ => None,
        }
    }

    fn offline_error(&self, msg: String) -> ServiceError {
        if self.target().is_remote() {
            ServiceError::target_offline(msg)
        } else {
            ServiceError::kernel_offline(msg)
        }
    }

    /// The kernel, starting (or reconnecting) it and waiting for it. Used by explicit actions
    /// (a run, an agent turn, a target switch); everything else uses [`Self::kernel`].
    pub async fn ensure_kernel(&self) -> ServiceResult<KernelClient> {
        if let Some(k) = self.kernel_client().await {
            return Ok(k);
        }
        if self.is_closed() {
            return Err(ServiceError::invalid("session is closed"));
        }
        let _starting = self.kernel.starting.lock().await;
        if let Some(k) = self.kernel_client().await {
            return Ok(k);
        }
        if self.is_closed() {
            return Err(ServiceError::invalid("session is closed"));
        }
        self.set_kernel_state(KernelState::Starting, None);
        match self.start_kernel_locked().await {
            Ok(k) => {
                if let Ok(mut f) = self.kernel.last_failure.lock() {
                    *f = None;
                }
                // `ready` after every start, then the session info with the active sandbox.
                self.set_kernel_state(KernelState::Ready, None);
                self.emit_session_changed().await;
                if let Some(rt) = self.arc() {
                    self.spawn(async move {
                        let _ = rt.refresh_index().await;
                    });
                }
                Ok(k)
            }
            Err(e) => {
                if let Ok(mut f) = self.kernel.last_failure.lock() {
                    *f = Some(std::time::Instant::now());
                }
                let state = if e.code == IpcErrorCode::TargetOffline {
                    KernelState::Offline
                } else {
                    KernelState::Error
                };
                self.set_kernel_state(state, Some(e.message.clone()));
                Err(e)
            }
        }
    }

    /// Start or reconnect the kernel. The caller holds `kernel.starting`.
    async fn start_kernel_locked(&self) -> ServiceResult<KernelClient> {
        let target = self.target();
        if target.is_remote() {
            // Bootstrap, mirror sync, and the session environment come before the first
            // kernel start on a remote target (R3).
            self.prepare_remote_if_needed().await?;
        } else if self.opts.kernel_launcher.is_none() {
            if self.opts.tooling.state() == ToolingState::Preparing {
                self.set_kernel_state(
                    KernelState::Starting,
                    Some("Setting up the Python tooling environment (first start)".into()),
                );
            }
            self.opts
                .tooling
                .wait()
                .await
                .map_err(ServiceError::kernel_offline)?;
        }
        let existing = self.kernel.client.read().await.clone();
        let started = match existing {
            Some(k) => {
                tokio::time::timeout(Duration::from_secs(90), k.reconnect(crate::CORE_VERSION))
                    .await
                    .map_err(|_| "kernel start timed out".to_string())
                    .and_then(|r| r.map(|_| (k, false)).map_err(|e| e.to_string()))
            }
            None => {
                let remote_no_network = match &target {
                    Target::Ssh { host, .. } => {
                        crate::egress::remote_bwrap_works(host, self.opts.control_dir.as_deref())
                            .await
                            .map_err(|e| self.offline_error(format!("kernel unavailable: {e:#}")))?
                    }
                    Target::Local => false,
                };
                let t = self.transport(remote_no_network);
                let isolation = t.network_isolation();
                if isolation == "none" {
                    tracing::warn!(
                        "the kernel on `{}` is not isolated from the network (no working bwrap or sandbox-exec)",
                        target.name()
                    );
                }
                if let Ok(mut s) = self.kernel.net_isolation.lock() {
                    *s = Some(isolation);
                }
                tracing::info!("kernel: {}", t.describe());
                tokio::time::timeout(
                    Duration::from_secs(90),
                    KernelClient::connect(t, crate::CORE_VERSION),
                )
                .await
                .map_err(|_| "kernel start timed out".to_string())
                .and_then(|r| r.map(|k| (k, true)).map_err(|e| e.to_string()))
            }
        };
        let (k, is_new) =
            started.map_err(|m| self.offline_error(format!("kernel unavailable: {m}")))?;
        if self.is_closed() {
            let _ = tokio::time::timeout(Duration::from_secs(5), k.shutdown()).await;
            return Err(ServiceError::invalid("session is closed"));
        }
        if is_new {
            self.kernel.generation.fetch_add(1, Ordering::SeqCst);
            *self.kernel.client.write().await = Some(k.clone());
            self.start_kernel_forwarder(k.clone());
        }
        Ok(k)
    }

    /// Start the kernel in the background unless a start is already running or one failed a
    /// moment ago.
    pub fn start_kernel_in_background(&self) {
        if self.is_closed() || self.kernel.starting.try_lock().is_err() {
            return;
        }
        let recent_failure = self
            .kernel
            .last_failure
            .lock()
            .ok()
            .and_then(|f| *f)
            .map(|t| t.elapsed() < START_BACKOFF)
            .unwrap_or(false);
        if recent_failure {
            return;
        }
        if let Some(rt) = self.arc() {
            self.spawn(async move {
                let _ = rt.ensure_kernel().await;
            });
        }
    }

    /// The kernel if it is connected. Otherwise this fails right away with `kernel_offline`
    /// or `target_offline` and starts the kernel in the background: commands never wait for a
    /// kernel start, the renderer retries on `kernel_state: ready` (IPC.md sections 5 and 7).
    pub async fn kernel(&self) -> ServiceResult<KernelClient> {
        if let Some(k) = self.kernel_client().await {
            return Ok(k);
        }
        if self.is_closed() {
            return Err(ServiceError::invalid("session is closed"));
        }
        let state = self.kernel_state();
        self.start_kernel_in_background();
        Err(self.offline_error(match state {
            KernelState::Starting => "the kernel is still starting".to_string(),
            _ => "the kernel is not running; it is being started".to_string(),
        }))
    }

    pub async fn shutdown_kernel(&self) {
        // Let a start in flight finish (briefly) so its client is not left behind.
        let _starting =
            tokio::time::timeout(Duration::from_secs(5), self.kernel.starting.lock()).await;
        let k = self.kernel.client.write().await.take();
        // The old client's forwarder must not outlive it (R7).
        self.kernel.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(h) = self.kernel.forwarder.lock().ok().and_then(|mut f| f.take()) {
            h.abort();
        }
        if let Some(k) = k {
            let _ = tokio::time::timeout(Duration::from_secs(5), k.shutdown()).await;
        }
        if let Ok(mut s) = self.kernel.state.lock() {
            *s = KernelState::Stopped;
        }
        self.emit(SessionEvent::KernelState {
            state: KernelState::Stopped,
            message: None,
            network_isolation: self.kernel_network_isolation(),
        });
    }

    /// Forward kernel notifications as run events and watch the connection. One forwarder per
    /// client: it stops when the client is replaced or the session closes.
    fn start_kernel_forwarder(&self, k: KernelClient) {
        let Some(rt) = self.arc() else { return };
        let generation = self.kernel.generation.load(Ordering::SeqCst);
        let mut rx = k.subscribe();
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut was_ready = true;
            let current =
                |rt: &SessionRuntime| rt.kernel.generation.load(Ordering::SeqCst) == generation;
            loop {
                if rt.is_closed() || !current(&rt) {
                    break;
                }
                tokio::select! {
                    n = rx.recv() => match n {
                        Ok(n) => rt.handle_notification(n).await,
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Output was dropped; the renderer reconciles through run_get.
                            rt.emit(SessionEvent::Resync);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = tick.tick() => {
                        let connected = k.is_connected().await;
                        if !current(&rt) {
                            break;
                        }
                        if was_ready && !connected && rt.kernel_state() == KernelState::Ready {
                            let remote = rt.target().is_remote();
                            rt.set_kernel_state(
                                if remote { KernelState::Offline } else { KernelState::Error },
                                Some("kernel connection lost".into()),
                            );
                            was_ready = false;
                            // One automatic reconnect attempt; later commands retry on demand.
                            let r = rt.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                let _ = r.ensure_kernel().await;
                            });
                        } else if connected {
                            was_ready = true;
                        }
                    }
                }
            }
        });
        if let Ok(mut f) = self.kernel.forwarder.lock() {
            if let Some(old) = f.replace(handle) {
                old.abort();
            }
        }
    }

    // ---- remote targets ----

    /// A kernel wheel to install on a remote host: a development build, one built earlier
    /// from the bundled source, or `None`.
    fn kernel_wheel(&self) -> Option<PathBuf> {
        if let Some(w) = target::bootstrap::find_dev_wheel() {
            return Some(w);
        }
        let dist = self.home.join("tooling").join("dist");
        let mut wheels: Vec<PathBuf> = std::fs::read_dir(dist)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "whl").unwrap_or(false))
            .collect();
        wheels.sort();
        wheels.pop()
    }

    /// Build a kernel wheel from the bundled kernel source into `<home>/tooling/dist`.
    async fn build_kernel_wheel(&self, logger: &TaskLogger) -> anyhow::Result<PathBuf> {
        let source = self
            .opts
            .kernel_source
            .clone()
            .or_else(target::bootstrap::find_dev_kernel_source)
            .ok_or_else(|| anyhow::anyhow!("no kernel source is available to build a wheel"))?;
        let dist = self.home.join("tooling").join("dist");
        std::fs::create_dir_all(&dist)?;
        logger
            .stream_command(
                // Building may fetch the build backend, so it names the configured index.
                &crate::egress::uv_install(
                    &crate::util::tool("uv"),
                    &["build"],
                    &[
                        "--wheel".to_string(),
                        "--out-dir".to_string(),
                        dist.to_string_lossy().to_string(),
                        source.to_string_lossy().to_string(),
                    ],
                ),
                None,
            )
            .await?;
        self.kernel_wheel()
            .ok_or_else(|| anyhow::anyhow!("building the kernel wheel produced no file"))
    }

    /// What the egress guard does for an install line when it knows the directory, done
    /// here because task commands are spawned without telling it: refuse a project that
    /// names its own package sources, and report a missing interpreter with the version to
    /// install instead of starting `uv`.
    async fn check_install(&self, line: &[String]) -> anyhow::Result<()> {
        if !line.iter().any(|a| a == crate::egress::INDEX_FLAG) {
            return Ok(());
        }
        let repo = self.repo.clone();
        tokio::task::spawn_blocking(move || {
            crate::egress::check_project_sources(&repo)?;
            crate::egress::require_python(&repo, None)
        })
        .await
        .map_err(|e| anyhow::anyhow!("install check failed: {e}"))?
    }

    /// The shared "prepare target" step for remote targets: tooling bootstrap, mirror sync,
    /// and the session environment (`uv sync` in the mirror). Runs before the first kernel
    /// start after create, open, and target changes (R3).
    async fn prepare_target(&self, logger: &TaskLogger) -> anyhow::Result<()> {
        let target = self.target();
        let Target::Ssh { host, .. } = &target else {
            return Ok(());
        };
        target::validate_host(host)?;
        let control = self.opts.control_dir.as_deref();
        logger.log(format!("checking the tooling on {host}"));
        let mut wheel = self.kernel_wheel();
        let outcome = match target::bootstrap::ensure(&target, control, wheel.as_deref()).await {
            Err(e) if wheel.is_none() => {
                logger.log(format!("{e:#}; building one from the bundled kernel"));
                wheel = Some(self.build_kernel_wheel(logger).await?);
                target::bootstrap::ensure(&target, control, wheel.as_deref()).await?
            }
            other => other?,
        };
        match outcome {
            target::bootstrap::Outcome::UpToDate(v) => {
                logger.log(format!("tooling {v} already installed"))
            }
            target::bootstrap::Outcome::Installed(v) => {
                logger.log(format!("installed tooling {v}"))
            }
        }
        logger.log("syncing the repository mirror");
        self.sync_if_needed()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        logger.log("preparing the session environment");
        let mirror = target.mirror_dir(&self.repo).unwrap_or_default();
        let index_flag = crate::egress::remote_index_flag("uv sync (remote)")?;
        let cmd = target::bootstrap::session_env_command(&mirror, &index_flag);
        let line = target::ssh_command(host, control, &cmd);
        logger.stream_command(&line, None).await?;
        Ok(())
    }

    async fn prepare_remote_if_needed(&self) -> ServiceResult<()> {
        let mut prepared = self.remote_prepared.lock().await;
        if *prepared {
            return Ok(());
        }
        let logger = TaskLogger::new(self.tasks.alloc("prepare-target"), self.events());
        let r = self.prepare_target(&logger).await;
        let err = r.as_ref().err().map(|e| format!("{e:#}"));
        logger.finish(r);
        match err {
            None => {
                *prepared = true;
                Ok(())
            }
            Some(m) => Err(ServiceError::target_offline(format!(
                "preparing the target failed: {m}"
            ))),
        }
    }

    // ---- runs ----

    fn run_record(&self, run_id: &str) -> Option<RunRecord> {
        self.runs.lock().ok().and_then(|r| r.get(run_id).cloned())
    }

    fn with_run<R>(&self, run_id: &str, f: impl FnOnce(&mut RunRecord) -> R) -> Option<R> {
        self.runs
            .lock()
            .ok()
            .and_then(|mut r| r.get_mut(run_id).map(f))
    }

    /// Insert a record and evict the oldest finished ones beyond [`MAX_FINISHED_RUNS`]; they
    /// stay available through the session store (`runs_list`, `run_get`).
    pub(super) fn insert_run(&self, rec: RunRecord) {
        let Ok(mut r) = self.runs.lock() else { return };
        r.insert(rec.meta.run_id.clone(), rec);
        let finished = r.values().filter(|x| x.finished).count();
        if finished > MAX_FINISHED_RUNS {
            let mut old: Vec<(String, String)> = r
                .values()
                .filter(|x| x.finished)
                .map(|x| (x.meta.started_at.clone(), x.meta.run_id.clone()))
                .collect();
            old.sort();
            for (_, id) in old.into_iter().take(finished - MAX_FINISHED_RUNS) {
                r.remove(&id);
            }
        }
    }

    /// Number of run records held in memory.
    pub fn runs_in_memory(&self) -> usize {
        self.runs.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// run id -> conversation for every agent run, read once from the transcripts.
    fn load_run_conversations(&self) {
        if self.run_conversations_loaded.swap(true, Ordering::SeqCst) {
            return;
        }
        let session = self.session_snapshot();
        let mut found: Vec<(String, String)> = Vec::new();
        for c in self.meta().conversations {
            if let Ok(events) = session.read_transcript(&c.id) {
                for e in events {
                    if let analyst_protocol::transcript::Event::Run { run_id, .. } = e {
                        found.push((run_id, c.id.clone()));
                    }
                }
            }
        }
        if let Ok(mut m) = self.run_conversations.lock() {
            for (run, conv) in found {
                m.entry(run).or_insert(conv);
            }
        }
    }

    /// The conversation whose agent turn started a run, if any.
    pub fn run_conversation(&self, run_id: &str) -> Option<String> {
        self.load_run_conversations();
        self.run_conversations
            .lock()
            .ok()
            .and_then(|m| m.get(run_id).cloned())
    }

    /// An agent turn of `conversation` started `run_id`. If the run was already announced
    /// without its conversation, it is announced again (the renderer upserts by run id).
    pub fn note_agent_run(&self, run_id: &str, conversation: &str) {
        if let Ok(mut m) = self.run_conversations.lock() {
            m.insert(run_id.to_string(), conversation.to_string());
        }
        let view = self.with_run(run_id, |r| {
            if r.conversation.is_none() {
                r.conversation = Some(conversation.to_string());
                Some(r.view())
            } else {
                None
            }
        });
        if let Some(Some(v)) = view {
            self.emit(SessionEvent::RunStarted { run: v });
        }
    }

    fn record_from_meta(&self, meta: RunMeta, source: RunSource) -> RunRecord {
        let finished = meta.status.is_finished();
        let conversation = self.run_conversation(&meta.run_id);
        RunRecord {
            meta,
            target: self.target().name().to_string(),
            stdout: String::new(),
            stderr: String::new(),
            artifacts: vec![],
            source,
            finished,
            conversation,
        }
    }

    /// A `RunView` for a run known only from the session store.
    fn stored_view(&self, r: analyst_protocol::session::SessionRun) -> RunView {
        RunView {
            conversation: self.run_conversation(&r.meta.run_id),
            meta: r.meta,
            target: r.target,
            artifacts: vec![],
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        }
    }

    /// Where this session keeps the output of remote runs (the kernel's own log files are on
    /// the host).
    fn run_store_dir(&self, run_id: &str) -> PathBuf {
        self.dir.join("runs").join(run_id)
    }

    /// Register a kernel run that was started elsewhere (agent tool, terminal through the
    /// kernel) when its first notification or `run.json` shows up.
    async fn register_kernel_run(&self, run_id: &str) -> Option<RunRecord> {
        let _g = self.run_registration.lock().await;
        if let Some(r) = self.run_record(run_id) {
            return Some(r);
        }
        let k = self.kernel_client().await?;
        let meta = k.run_status(run_id).await.ok()?;
        if meta.run_id != run_id {
            return None;
        }
        let rec = self.record_from_meta(meta, RunSource::Kernel);
        self.insert_run(rec.clone());
        if let Ok(s) = self.session.lock() {
            let _ = s.append_run(&rec.meta);
        }
        self.emit(SessionEvent::RunStarted { run: rec.view() });
        if rec.finished {
            self.emit(SessionEvent::RunDone { run: rec.view() });
        }
        Some(rec)
    }

    async fn handle_notification(&self, n: KernelNotification) {
        let Some(run_id) = n.run_id().map(|s| s.to_string()) else {
            return;
        };
        // Run ids become path components (session store, pulled artifacts).
        if !figures::safe_run_id(&run_id) {
            tracing::warn!("ignoring a kernel notification with an unusable run id");
            return;
        }
        if self.run_record(&run_id).is_none() && self.register_kernel_run(&run_id).await.is_none() {
            return;
        }
        let remote = self.target().is_remote();
        match n {
            KernelNotification::Stdout(o) => {
                self.with_run(&run_id, |r| push_bounded(&mut r.stdout, &o.chunk));
                if remote {
                    append_log(&self.run_store_dir(&run_id).join("stdout.log"), &o.chunk);
                }
                self.emit(SessionEvent::RunOutput {
                    run_id,
                    stream: OutputStream::Stdout,
                    chunk: o.chunk,
                });
            }
            KernelNotification::Stderr(o) => {
                self.with_run(&run_id, |r| push_bounded(&mut r.stderr, &o.chunk));
                if remote {
                    append_log(&self.run_store_dir(&run_id).join("stderr.log"), &o.chunk);
                }
                self.emit(SessionEvent::RunOutput {
                    run_id,
                    stream: OutputStream::Stderr,
                    chunk: o.chunk,
                });
            }
            KernelNotification::Artifact(a) => {
                // Paths from the kernel are normalised before they are joined to anything.
                let mut artifact = a.artifact;
                match normalize_rel_path(&artifact.path) {
                    Ok(p) => artifact.path = p,
                    Err(e) => {
                        tracing::warn!("ignoring an artifact with an unusable path: {e}");
                        return;
                    }
                }
                let script = self
                    .with_run(&run_id, |r| {
                        r.artifacts.retain(|x| x.path != artifact.path);
                        r.artifacts.push(artifact.clone());
                        r.meta.script.clone()
                    })
                    .unwrap_or_default();
                let figure = if artifact.kind == ArtifactKind::Figure {
                    resolve_in_repo(&self.repo, &artifact.path, true)
                        .ok()
                        .and_then(|_| {
                            figures::figure_view(
                                &self.repo,
                                &self.repo.join(&artifact.path),
                                &run_id,
                                &script,
                            )
                        })
                        .or_else(|| figures::figure_view_from_artifact(&artifact, &run_id, &script))
                } else {
                    None
                };
                self.emit(SessionEvent::RunArtifact {
                    run_id,
                    artifact,
                    figure,
                });
            }
            KernelNotification::Done(d) => {
                let meta = match self.kernel_client().await {
                    Some(k) => k.run_status(&run_id).await.ok(),
                    None => None,
                };
                let view = self.with_run(&run_id, |r| {
                    match meta {
                        Some(m) if m.run_id == r.meta.run_id => r.meta = m,
                        _ => {
                            r.meta.status = d.status;
                            r.meta.exit_code = d.exit_code;
                            r.meta.finished_at = Some(now_rfc3339());
                        }
                    }
                    r.finished = true;
                    r.view()
                });
                let Some(v) = view else { return };
                if let Ok(s) = self.session.lock() {
                    let _ = s.append_run(&v.meta);
                }
                if !remote {
                    self.emit(SessionEvent::RunDone { run: v });
                    return;
                }
                // Remote: keep the artifact list with the session, and pull the rendered
                // artifacts once, off this task (G8). Agent runs are pulled by the tool that
                // started them, so they are not pulled again here.
                let store = self.run_store_dir(&v.meta.run_id);
                let _ = std::fs::create_dir_all(&store);
                let lines: String = v
                    .artifacts
                    .iter()
                    .filter_map(|a| serde_json::to_string(a).ok())
                    .map(|l| l + "\n")
                    .collect();
                let _ = std::fs::write(store.join("artifacts.jsonl"), lines);
                if v.conversation.is_some() {
                    self.emit(SessionEvent::RunDone { run: v });
                } else if let Some(rt) = self.arc() {
                    self.spawn(async move {
                        rt.pull_run_artifacts(&v).await;
                        rt.emit(SessionEvent::RunDone { run: v });
                    });
                }
            }
            KernelNotification::Other { .. } => {}
        }
    }

    /// On a remote target, pull the run's rendered artifacts (figures, their sidecars,
    /// `run.json`; nothing else leaves the host) into the repo and the session.
    async fn pull_run_artifacts(&self, v: &RunView) {
        let target = self.target();
        let (Target::Ssh { host, .. }, Some(mirror)) = (&target, target.mirror_dir(&self.repo))
        else {
            return;
        };
        let out_dir = normalize_rel_path(&v.meta.out_dir).unwrap_or_default();
        let paths: Vec<String> = target::sync::rendered_paths(&v.artifacts, &out_dir)
            .into_iter()
            .filter_map(|p| normalize_rel_path(&p).ok())
            .filter(|p| !crate::util::is_reserved_path(p))
            .collect();
        if let Err(e) = target::sync::pull_rendered(
            host,
            &mirror,
            &paths,
            &self.repo,
            self.opts.control_dir.as_deref(),
        )
        .await
        {
            tracing::warn!("pulling run {} failed: {e}", v.meta.run_id);
            return;
        }
        let session = self.session_snapshot();
        for a in v.artifacts.iter().filter(|a| a.rendered) {
            let Ok(src) = resolve_in_repo(&self.repo, &a.path, true) else {
                continue;
            };
            let dst = session.pulled_artifact_path(&v.meta.run_id, &a.path);
            if let Some(p) = dst.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let _ = std::fs::copy(&src, &dst);
        }
    }

    /// Sync the mirror when the target is remote. Returns the tree hash. Syncs are
    /// serialised.
    pub async fn sync_if_needed(&self) -> ServiceResult<Option<String>> {
        let target = self.target();
        if !target.is_remote() {
            return Ok(None);
        }
        let mut last = self.last_sync_hash.lock().await;
        // Changes made while the push runs make the mirror dirty again.
        self.mirror_dirty.store(false, Ordering::SeqCst);
        let report = match target::sync::push(
            &self.repo,
            &target,
            self.opts.control_dir.as_deref(),
            last.as_deref(),
            false,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                self.mirror_dirty.store(true, Ordering::SeqCst);
                return Err(ServiceError::target_offline(format!("sync failed: {e:#}")));
            }
        };
        if let Some(w) = &report.warning {
            tracing::warn!("{w}");
        }
        *last = Some(report.tree_hash.clone());
        if let Ok(mut t) = self.last_mirror_sync.lock() {
            *t = Some(std::time::Instant::now());
        }
        if let Ok(mut s) = self.session.lock() {
            let _ = s.set_last_tree_hash(Some(report.tree_hash.clone()));
        }
        Ok(Some(report.tree_hash))
    }

    /// Bring the mirror up to date before a request that reads it (index, checks, equations)
    /// on a remote target (R4). Nothing happens when no local file changed since the last
    /// sync; syncs are at least [`SYNC_DEBOUNCE`] apart.
    pub async fn sync_debounced(&self) -> ServiceResult<()> {
        if !self.target().is_remote() {
            return Ok(());
        }
        let watcher_broken = self
            .watcher_error
            .lock()
            .map(|e| e.is_some())
            .unwrap_or(false);
        if !watcher_broken && !self.mirror_dirty.load(Ordering::SeqCst) {
            return Ok(());
        }
        let since = self
            .last_mirror_sync
            .lock()
            .ok()
            .and_then(|t| *t)
            .map(|t| t.elapsed());
        if let Some(e) = since {
            if e < SYNC_DEBOUNCE {
                tokio::time::sleep(SYNC_DEBOUNCE - e).await;
                if !watcher_broken && !self.mirror_dirty.load(Ordering::SeqCst) {
                    return Ok(()); // another request synced meanwhile
                }
            }
        }
        self.sync_if_needed().await.map(|_| ())
    }

    pub async fn run_start(&self, script: &str, args: Vec<String>) -> ServiceResult<RunView> {
        let script = normalize_rel_path(script).map_err(ServiceError::invalid)?;
        let script_abs =
            resolve_in_repo(&self.repo, &script, true).map_err(ServiceError::not_found)?;
        if !script_abs.is_file() {
            return Err(ServiceError::not_found(format!(
                "script `{script}` not found"
            )));
        }
        // A run is an explicit action: wait for the kernel (and, on a remote target, for the
        // target to be prepared) instead of failing while it starts.
        let k = self.ensure_kernel().await?;
        let tree_hash = match self.sync_if_needed().await? {
            Some(h) => Some(h),
            None => {
                let repo = self.repo.clone();
                tokio::task::spawn_blocking(move || target::sync::tree_id(&repo).ok())
                    .await
                    .ok()
                    .flatten()
            }
        };
        let eff = config::effective_project(&self.project(), self.target().name());
        let _g = self.run_registration.lock().await;
        let started = k
            .run_start(RunStartParams {
                script: script.clone(),
                args: Some(args.clone()),
                env: None,
                timeout_s: Some(eff.timeout_s as f64),
                tree_hash,
            })
            .await?;
        if !figures::safe_run_id(&started.run_id) {
            return Err(ServiceError::internal(
                "the kernel returned an unusable run id",
            ));
        }
        let meta = match k.run_status(&started.run_id).await {
            Ok(m) if m.run_id == started.run_id => m,
            _ => RunMeta {
                run_id: started.run_id.clone(),
                script: script.clone(),
                args: args.clone(),
                status: RunStatus::Running,
                exit_code: None,
                started_at: now_rfc3339(),
                finished_at: None,
                out_dir: started.out_dir.clone(),
                sandbox: "unknown".into(),
                tree_hash: None,
                pid: None,
                host: "local".into(),
            },
        };
        let rec = self.record_from_meta(meta, RunSource::Kernel);
        self.insert_run(rec.clone());
        if let Ok(s) = self.session.lock() {
            let _ = s.append_run(&rec.meta);
        }
        let view = rec.view();
        self.emit(SessionEvent::RunStarted { run: view.clone() });
        Ok(view)
    }

    pub async fn run_cancel(&self, run_id: &str) -> ServiceResult<()> {
        let k = self.kernel().await?;
        let r = k.run_cancel(run_id).await?;
        if !r.ok {
            return Err(ServiceError::not_found(format!(
                "run `{run_id}` is not running"
            )));
        }
        Ok(())
    }

    /// Runs known to this process plus the session's run history, newest first. Needs no
    /// kernel.
    pub fn runs_list(&self, limit: Option<u32>) -> Vec<RunView> {
        let mut views: Vec<RunView> = self
            .runs
            .lock()
            .map(|r| r.values().map(|v| v.view()).collect())
            .unwrap_or_default();
        let mut seen: BTreeSet<String> = views.iter().map(|v| v.meta.run_id.clone()).collect();
        if let Ok(runs) = self.session_snapshot().read_runs() {
            for r in runs.into_iter().rev() {
                if seen.insert(r.meta.run_id.clone()) {
                    views.push(self.stored_view(r));
                }
            }
        }
        views.sort_by(|a, b| b.meta.started_at.cmp(&a.meta.started_at));
        views.truncate(limit.unwrap_or(50).max(1) as usize);
        views
    }

    /// A run with its output: any run `runs_list` shows, also after a restart. The text comes
    /// from the run's log files (the kernel's for local runs, the session store for remote
    /// ones); each stream is cut to its last [`RUN_GET_MAX_BYTES`] with a note. Needs no
    /// kernel.
    pub fn run_get(&self, run_id: &str) -> ServiceResult<RunDetail> {
        if !figures::safe_run_id(run_id) {
            return Err(ServiceError::not_found(format!("run `{run_id}` not found")));
        }
        let record = self.run_record(run_id);
        let mut view = match &record {
            Some(r) => r.view(),
            None => {
                let run = self
                    .session_snapshot()
                    .find_run(run_id)?
                    .ok_or_else(|| ServiceError::not_found(format!("run `{run_id}` not found")))?;
                self.stored_view(run)
            }
        };
        let dirs = [
            self.home.join("runs").join(run_id),
            self.run_store_dir(run_id),
        ];
        let read = |name: &str| {
            dirs.iter()
                .find_map(|d| read_log_tail(&d.join(name), RUN_GET_MAX_BYTES))
        };
        let stdout = read("stdout.log")
            .or_else(|| record.as_ref().map(|r| r.stdout.clone()))
            .unwrap_or_default();
        let stderr = read("stderr.log")
            .or_else(|| record.as_ref().map(|r| r.stderr.clone()))
            .unwrap_or_default();
        if view.artifacts.is_empty() {
            view.artifacts = dirs
                .iter()
                .find_map(|d| {
                    crate::session::read_jsonl::<Artifact>(&d.join("artifacts.jsonl"))
                        .ok()
                        .filter(|a| !a.is_empty())
                })
                .unwrap_or_default()
                .into_iter()
                .filter(|a| normalize_rel_path(&a.path).is_ok())
                .collect();
        }
        if record.is_none() {
            view.stdout_tail = tail(&stdout, TAIL_BYTES);
            view.stderr_tail = tail(&stderr, TAIL_BYTES);
        }
        Ok(RunDetail {
            view,
            stdout,
            stderr,
        })
    }

    // ---- index and checks ----

    /// The cached index. Without a cache this needs a connected kernel: when the kernel is
    /// not ready it fails at once with `kernel_offline` / `target_offline` and the kernel is
    /// started in the background; `index_changed` follows when the index is built.
    pub async fn index_get(&self) -> ServiceResult<IndexResult> {
        if let Some(i) = self.index_cache.read().await.clone() {
            return Ok(i);
        }
        self.kernel().await?;
        self.refresh_index().await
    }

    /// Re-index with the connected kernel (does not start it). Emits `index_changed`.
    pub async fn refresh_index(&self) -> ServiceResult<IndexResult> {
        let k = self
            .kernel_client()
            .await
            .ok_or_else(|| self.offline_error("kernel is not connected".into()))?;
        self.sync_debounced().await?;
        let project = self.project();
        let scripts = k
            .scripts_index(Some(project.project.scripts.clone()))
            .await?;
        let lib = k.lib_index(Some(project.project.library.clone())).await?;
        let result = IndexResult {
            scripts: scripts.scripts,
            library: lib.functions,
        };
        *self.index_cache.write().await = Some(result.clone());
        self.emit(SessionEvent::IndexChanged);
        Ok(result)
    }

    /// Every `.py` under the script globs and the library (for clearing stale diagnostics).
    fn all_py_paths(&self) -> Vec<String> {
        let project = self.project();
        let mut out = BTreeSet::new();
        let mut roots: Vec<String> = project
            .project
            .scripts
            .iter()
            .map(|g| {
                g.split(['*', '?', '['])
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();
        roots.push(project.project.library.replace('.', "/"));
        for root in roots {
            let base = self.repo.join(&root);
            for e in walkdir::WalkDir::new(&base)
                .into_iter()
                .filter_entry(|e| {
                    let n = e.file_name().to_string_lossy();
                    n != "__pycache__" && n != ".venv"
                })
                .flatten()
            {
                if e.file_type().is_file()
                    && e.path().extension().map(|x| x == "py").unwrap_or(false)
                {
                    if let Ok(rel) = e.path().strip_prefix(&self.repo) {
                        out.insert(rel.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    /// Contract check + lint; also emits `diagnostics` for the affected paths.
    pub async fn check_run(&self, paths: Option<Vec<String>>) -> ServiceResult<Vec<Diagnostic>> {
        let k = self.kernel().await?;
        self.sync_debounced().await?;
        let paths = paths.map(|ps| {
            ps.into_iter()
                .filter_map(|p| normalize_rel_path(&p).ok())
                .collect::<Vec<_>>()
        });
        let mut diags = k.contract_check(paths.clone()).await?.diagnostics;
        match k.lint_run(paths.clone()).await {
            Ok(r) => diags.extend(r.diagnostics),
            Err(e) => tracing::warn!("lint.run failed: {e}"),
        }
        diags.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
        let mut affected: BTreeSet<String> = match &paths {
            Some(p) => p.iter().cloned().collect(),
            None => self.all_py_paths().into_iter().collect(),
        };
        for d in &diags {
            affected.insert(d.path.clone());
        }
        self.emit(SessionEvent::Diagnostics {
            paths: affected.into_iter().collect(),
            diagnostics: diags.clone(),
        });
        Ok(diags)
    }

    // ---- outputs, equations, packages ----

    pub async fn out_list(
        &self,
        path: Option<String>,
        run_id: Option<String>,
    ) -> ServiceResult<OutListResult> {
        // Paths are normalised here, before they reach the kernel.
        let path = match path {
            Some(p) if !p.trim().is_empty() && p.trim() != "." => {
                Some(normalize_rel_path(&p).map_err(ServiceError::invalid)?)
            }
            _ => None,
        };
        if let Some(r) = &run_id {
            if !figures::safe_run_id(r) {
                return Err(ServiceError::invalid(format!("`{r}` is not a run id")));
            }
        }
        let k = self.kernel().await?;
        let mut result = k.out_list(OutListParams { path, run_id }).await?;
        // ... and what comes back is normalised before anyone joins it to a directory.
        result
            .entries
            .retain(|e| normalize_rel_path(&e.path).is_ok());
        Ok(result)
    }

    pub async fn out_read(
        &self,
        path: String,
        max_bytes: Option<u64>,
    ) -> ServiceResult<OutReadResult> {
        let path = normalize_rel_path(&path).map_err(ServiceError::invalid)?;
        let k = self.kernel().await?;
        Ok(k.out_read(OutReadParams { path, max_bytes }).await?)
    }

    pub async fn eq_extract(&self, path: &str) -> ServiceResult<Vec<Equation>> {
        let path = normalize_rel_path(path).map_err(ServiceError::invalid)?;
        let k = self.kernel().await?;
        self.sync_debounced().await?;
        Ok(k.eq_extract(EqExtractParams {
            path,
            function: None,
        })
        .await?
        .equations)
    }

    pub async fn eq_check(&self, path: &str, function: &str) -> ServiceResult<EqCheckResult> {
        let path = normalize_rel_path(path).map_err(ServiceError::invalid)?;
        let k = self.kernel().await?;
        self.sync_debounced().await?;
        Ok(k.eq_check(EqCheckParams {
            path,
            function: function.to_string(),
            trials: None,
            tol: None,
            ranges: None,
        })
        .await?)
    }

    pub async fn env_packages(&self) -> ServiceResult<Vec<PackageInfo>> {
        let k = self.kernel().await?;
        Ok(k.env_packages().await?.packages)
    }

    /// `uv add|remove|sync` as a task; `packages_changed` on success.
    ///
    /// The local repo is canonical, also for the environment: on a remote target the change
    /// is made to the local `pyproject.toml` and `uv.lock` first, then the mirror is synced,
    /// then the host runs `uv sync` (R5). Changing only the mirror would be undone by the
    /// next push.
    pub fn env_task(&self, op: &str, packages: Vec<String>) -> ServiceResult<TaskId> {
        if op != "sync" && packages.is_empty() {
            return Err(ServiceError::invalid(
                "`packages` must list at least one package",
            ));
        }
        if let Some(bad) = packages
            .iter()
            .find(|p| p.trim().is_empty() || p.trim_start().starts_with('-'))
        {
            return Err(ServiceError::invalid(format!(
                "`{bad}` is not a package requirement"
            )));
        }
        let policy = self.effective_policy();
        if policy.packages == analyst_protocol::config::AskPolicy::Deny {
            return Err(ServiceError::policy_denied(
                "package changes are denied by policy",
            ));
        }
        if !matches!(op, "add" | "remove" | "sync") {
            return Err(ServiceError::invalid(format!(
                "unknown package operation `{op}`"
            )));
        }
        let has_pyproject = self.repo.join("pyproject.toml").is_file();
        if op == "sync" && !has_pyproject {
            return Err(ServiceError::invalid(
                "env_sync needs a pyproject.toml / uv.lock",
            ));
        }
        let op = op.to_string();
        let id = self.tasks.alloc(&format!("env-{op}"));
        let logger = TaskLogger::new(id.clone(), self.events());
        let Some(rt) = self.arc() else {
            return Err(ServiceError::internal("session runtime gone"));
        };
        let target = self.target();
        let uv = crate::util::tool("uv");
        self.spawn(async move {
            let r: anyhow::Result<()> = async {
                match &target {
                    Target::Local => {
                        // Installs name the configured index (and are authorized when the
                        // line is spawned); an uninstall needs no index and runs offline.
                        let mut rest: Vec<String> = Vec::new();
                        if !has_pyproject {
                            rest.extend(["--python".into(), rt.session_python()]);
                        }
                        if op != "sync" {
                            rest.push("--".into());
                            rest.extend(packages);
                        }
                        let line = match (op.as_str(), has_pyproject) {
                            ("add", true) => crate::egress::uv_install(&uv, &["add"], &rest),
                            ("remove", true) => crate::egress::uv_install(&uv, &["remove"], &rest),
                            ("sync", _) => crate::egress::uv_install(&uv, &["sync"], &rest),
                            ("add", false) => {
                                crate::egress::uv_install(&uv, &["pip", "install"], &rest)
                            }
                            _ => {
                                let mut line = vec![uv, "pip".into(), "uninstall".into()];
                                line.extend(rest);
                                line
                            }
                        };
                        rt.check_install(&line).await?;
                        logger.stream_command(&line, Some(&rt.repo)).await
                    }
                    Target::Ssh { host, .. } => {
                        let control = rt.opts.control_dir.as_deref();
                        let mirror = target.mirror_dir(&rt.repo).unwrap_or_default();
                        if has_pyproject {
                            // 1. Change the canonical project files locally (no local venv).
                            let mut rest: Vec<String> = Vec::new();
                            if op != "sync" {
                                rest.extend(["--no-sync".into(), "--".into()]);
                                rest.extend(packages);
                            }
                            let sub = match op.as_str() {
                                "add" => "add",
                                "remove" => "remove",
                                _ => "lock",
                            };
                            // Resolving needs the index even when nothing is installed here.
                            let line = crate::egress::uv_install(&uv, &[sub], &rest);
                            rt.check_install(&line).await?;
                            logger.stream_command(&line, Some(&rt.repo)).await?;
                            // 2. Push them, 3. bring the remote environment in line.
                            logger.log("syncing the repository mirror");
                            rt.mirror_dirty.store(true, Ordering::SeqCst);
                            rt.sync_if_needed()
                                .await
                                .map_err(|e| anyhow::anyhow!("{e}"))?;
                            let index_flag = crate::egress::remote_index_flag("uv sync (remote)")?;
                            let cmd = target::bootstrap::session_env_command(&mirror, &index_flag);
                            let line = target::ssh_command(host, control, &cmd);
                            logger.stream_command(&line, None).await
                        } else {
                            // No project file to keep canonical: change the remote venv.
                            rt.sync_if_needed()
                                .await
                                .map_err(|e| anyhow::anyhow!("{e}"))?;
                            let pkgs: Vec<String> =
                                packages.iter().map(|p| crate::util::sh_quote(p)).collect();
                            let uv_call = if op == "add" {
                                format!(
                                    "~/.analyst/bin/uv pip install {}",
                                    crate::egress::remote_index_flag("uv pip install (remote)")?
                                )
                            } else {
                                "UV_OFFLINE=1 ~/.analyst/bin/uv pip uninstall".to_string()
                            };
                            let remote = format!(
                                "cd {} && {uv_call} --python .venv/bin/python -- {}",
                                crate::util::sh_quote_path(&mirror),
                                pkgs.join(" ")
                            );
                            let line = target::ssh_command(host, control, &remote);
                            logger.stream_command(&line, None).await
                        }
                    }
                }
            }
            .await;
            let ok = r.is_ok();
            logger.finish(r);
            if ok {
                rt.emit(SessionEvent::PackagesChanged);
            }
        });
        Ok(id)
    }

    /// Re-bind the session to another target, prepare it (bootstrap, sync, session env on
    /// remote hosts), restart the kernel, and report `session_changed` when done.
    pub fn set_target_task(&self, target_name: &str) -> ServiceResult<TaskId> {
        let cfg = config::find_target(&self.global(), target_name)
            .map_err(|e| ServiceError::not_found(e.to_string()))?;
        let new_target =
            Target::from_config(&cfg).map_err(|e| ServiceError::invalid(e.to_string()))?;
        let id = self.tasks.alloc("set-target");
        let logger = TaskLogger::new(id.clone(), self.events());
        let Some(rt) = self.arc() else {
            return Err(ServiceError::internal("session runtime gone"));
        };
        self.spawn(async move {
            let r: anyhow::Result<()> = async {
                logger.log(format!("switching target to {}", new_target.name()));
                // Stops the old client and its event forwarder (R7).
                rt.shutdown_kernel().await;
                if let Ok(mut t) = rt.target.write() {
                    *t = new_target.clone();
                }
                *rt.last_sync_hash.lock().await = None;
                *rt.remote_prepared.lock().await = false;
                rt.mirror_dirty.store(true, Ordering::SeqCst);
                if let Ok(mut t) = rt.last_mirror_sync.lock() {
                    *t = None;
                }
                if let Ok(mut f) = rt.kernel.last_failure.lock() {
                    *f = None;
                }
                *rt.index_cache.write().await = None;
                if let Ok(mut s) = rt.session.lock() {
                    s.record.target = new_target.name().to_string();
                    s.save()?;
                }
                if new_target.is_remote() {
                    rt.prepare_target(&logger).await?;
                    *rt.remote_prepared.lock().await = true;
                } else if rt.repo.join("pyproject.toml").is_file() && crate::init::has_uv() {
                    let line = crate::egress::uv_install::<String>(
                        &crate::util::tool("uv"),
                        &["sync"],
                        &[],
                    );
                    rt.check_install(&line).await?;
                    logger.stream_command(&line, Some(&rt.repo)).await?;
                }
                rt.ensure_kernel()
                    .await
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!("{}", e.message))
            }
            .await;
            logger.finish(r);
            // Target name, kind, host, kernel state, and sandbox as they are now.
            rt.emit_session_changed().await;
        });
        Ok(id)
    }

    // ---- figures ----

    fn synthesized_view(&self, s: &ScannedRun) -> RunView {
        RunView {
            conversation: None,
            meta: RunMeta {
                run_id: s.run_id.clone(),
                script: s.script.clone(),
                args: s.args.clone(),
                status: RunStatus::Done,
                exit_code: None,
                started_at: s.started_at.clone(),
                finished_at: None,
                out_dir: s.dir_rel.clone(),
                sandbox: "none".into(),
                tree_hash: None,
                pid: None,
                host: s.host.clone().unwrap_or_else(|| "local".into()),
            },
            target: self.target().name().to_string(),
            artifacts: vec![],
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        }
    }

    /// Figures grouped by run, newest run first, including terminal runs found under `out/`
    /// and artifacts pulled from remote targets. Runs on the blocking pool; needs no kernel.
    pub async fn figures_list(&self, limit: Option<u32>) -> Vec<FigureGroup> {
        let Some(rt) = self.arc() else { return vec![] };
        tokio::task::spawn_blocking(move || rt.figures_list_blocking(limit))
            .await
            .unwrap_or_default()
    }

    /// The scan behind [`Self::figures_list`]. The run history is read once, and scanning
    /// stops at the limit (U3).
    pub fn figures_list_blocking(&self, limit: Option<u32>) -> Vec<FigureGroup> {
        let limit = limit.unwrap_or(100).max(1) as usize;
        let out_dir = self.out_dir();
        let session = self.session_snapshot();
        // Latest record per run id, and the order of first appearance for the pulled scan.
        let mut stored: HashMap<String, analyst_protocol::session::SessionRun> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for r in session.read_runs().unwrap_or_default() {
            if !stored.contains_key(&r.meta.run_id) {
                order.push(r.meta.run_id.clone());
            }
            stored.insert(r.meta.run_id.clone(), r);
        }
        let mut groups: Vec<FigureGroup> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for scanned in figures::scan_out_limited(&self.repo, &out_dir, limit) {
            let run = match self.run_record(&scanned.run_id) {
                Some(r) => r.view(),
                None => match stored.get(&scanned.run_id) {
                    Some(r) => self.stored_view(r.clone()),
                    None => self.synthesized_view(&scanned),
                },
            };
            seen.insert(scanned.run_id.clone());
            groups.push(FigureGroup {
                run,
                figures: scanned.figures,
            });
        }
        // Remote runs whose figures were pulled into the session artifacts.
        let artifacts_dir = session.artifacts_dir();
        if artifacts_dir.is_dir() {
            let mut pulled = 0usize;
            for id in order.iter().rev() {
                if pulled >= limit {
                    break;
                }
                if seen.contains(id) || !artifacts_dir.join(id).is_dir() {
                    continue;
                }
                let Some(r) = stored.get(id) else { continue };
                let figs = figures::scan_pulled(&artifacts_dir, id, &r.meta.script);
                if figs.is_empty() {
                    continue;
                }
                seen.insert(id.clone());
                pulled += 1;
                let run = self
                    .run_record(id)
                    .map(|rec| rec.view())
                    .unwrap_or_else(|| self.stored_view(r.clone()));
                groups.push(FigureGroup { run, figures: figs });
            }
        }
        groups.sort_by(|a, b| b.run.meta.started_at.cmp(&a.run.meta.started_at));
        groups.truncate(limit);
        groups
    }

    /// Bytes of a figure (or any file) under the repo; pulled from a remote target on first
    /// access and cached under the session's `artifacts/`.
    pub async fn figure_data(&self, path: &str) -> ServiceResult<FigureData> {
        let (rel, local) = fs::resolve(&self.repo, path)?;
        if rel.is_empty() {
            return Err(ServiceError::invalid("`path` must name a file"));
        }
        let bytes = match fs::read_binary(&self.repo, &rel) {
            Ok((_, b)) => b,
            Err(e) if e.code == IpcErrorCode::NotFound && !local.exists() => {
                let target = self.target();
                let (Target::Ssh { host, .. }, Some(mirror)) =
                    (&target, target.mirror_dir(&self.repo))
                else {
                    return Err(ServiceError::not_found(format!("`{rel}` not found")));
                };
                // Only rendered output is ever pulled from a host (G8).
                let out_prefix = format!("{}/", self.out_dir().trim_matches('/'));
                if !rel.starts_with(&out_prefix) || FigureFormat::from_path(&rel).is_none() {
                    return Err(ServiceError::not_found(format!("`{rel}` not found")));
                }
                let session = self.session_snapshot();
                let run_id = figures::run_dir_of(&self.out_dir(), &rel)
                    .and_then(|d| figures::read_run_json(&self.repo.join(d).join("run.json")))
                    .map(|j| j.run_id)
                    .filter(|id| figures::safe_run_id(id))
                    .unwrap_or_else(|| "pulled".into());
                let dst = session.pulled_artifact_path(&run_id, &rel);
                if !dst.is_file() {
                    target::sync::pull_artifact(
                        host,
                        &mirror,
                        &rel,
                        &dst,
                        self.opts.control_dir.as_deref(),
                    )
                    .await
                    .map_err(|e| ServiceError::target_offline(format!("pulling `{rel}`: {e:#}")))?;
                }
                std::fs::read(&dst)?
            }
            Err(e) => return Err(e),
        };
        let mime = FigureFormat::from_path(&rel)
            .map(|f| f.mime().to_string())
            .unwrap_or_else(|| guess_mime(&rel).to_string());
        Ok(FigureData {
            mime,
            base64: fs::to_base64(&bytes),
        })
    }

    // ---- filesystem ----

    pub fn fs_tree(&self, path: Option<&str>, depth: Option<u32>) -> ServiceResult<Vec<FileEntry>> {
        fs::tree(&self.repo, path, depth, &self.out_dir())
    }

    pub fn fs_read(&self, path: &str) -> ServiceResult<FileContent> {
        fs::read(&self.repo, path)
    }

    pub fn fs_read_binary(&self, path: &str) -> ServiceResult<FigureData> {
        let (rel, bytes) = fs::read_binary(&self.repo, path)?;
        Ok(FigureData {
            mime: guess_mime(&rel).to_string(),
            base64: fs::to_base64(&bytes),
        })
    }

    /// Editor save: writes immediately; `.py` files are checked afterwards (`diagnostics`).
    pub fn fs_write(
        &self,
        path: &str,
        content: &str,
        expected_hash: Option<&str>,
    ) -> ServiceResult<WriteResult> {
        let r = fs::write(&self.repo, path, content, expected_hash)?;
        self.mirror_dirty.store(true, Ordering::SeqCst);
        if path.ends_with(".py") {
            if let (Some(rt), Ok(rel)) = (self.arc(), normalize_rel_path(path)) {
                self.spawn(async move {
                    if rt.kernel_client().await.is_some() {
                        let _ = rt.check_run(Some(vec![rel])).await;
                    }
                });
            }
        }
        Ok(r)
    }

    pub fn fs_create(&self, path: &str, is_dir: bool) -> ServiceResult<()> {
        fs::create(&self.repo, path, is_dir)
    }

    pub fn fs_delete(&self, path: &str) -> ServiceResult<()> {
        fs::delete(&self.repo, path)
    }

    pub fn fs_rename(&self, from: &str, to: &str) -> ServiceResult<()> {
        fs::rename(&self.repo, from, to)
    }

    pub async fn fs_search(
        &self,
        pattern: &str,
        glob: Option<&str>,
        max_results: Option<u32>,
    ) -> ServiceResult<Vec<SearchHit>> {
        fs::search(&self.repo, pattern, glob, max_results).await
    }

    // ---- conversations ----

    pub fn conversation(&self, id: &str) -> Option<Arc<ConversationRuntime>> {
        self.conversations
            .lock()
            .ok()
            .and_then(|c| c.get(id).cloned())
    }

    /// The conversation runtime, created on first use for a known conversation id.
    pub fn conversation_runtime(&self, id: &str) -> ServiceResult<Arc<ConversationRuntime>> {
        if let Some(c) = self.conversation(id) {
            return Ok(c);
        }
        let meta = self.meta();
        if !meta.conversations.iter().any(|c| c.id == id) {
            return Err(ServiceError::not_found(format!(
                "conversation `{id}` not found"
            )));
        }
        // Two callers may race here; both must get the same runtime.
        let mut m = self
            .conversations
            .lock()
            .map_err(|_| ServiceError::internal("conversations poisoned"))?;
        Ok(m.entry(id.to_string())
            .or_insert_with(|| Arc::new(ConversationRuntime::new(id)))
            .clone())
    }

    pub fn conversation_meta(&self, id: &str) -> ServiceResult<ConversationMeta> {
        self.meta()
            .conversations
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| ServiceError::not_found(format!("conversation `{id}` not found")))
    }

    pub fn conversation_new(&self, title: Option<&str>) -> ServiceResult<ConversationSummary> {
        let id = crate::util::stamped_id();
        let model = self.summary().model;
        let created_at = now_rfc3339();
        let title = title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Conversation {}", self.meta().conversations.len() + 1));
        let meta = ConversationMeta {
            id: id.clone(),
            title: title.clone(),
            model: None,
            mode: Mode::Build,
            created_at: created_at.clone(),
        };
        self.update_meta(|m| {
            m.conversations.push(meta);
            m.active_conversation = Some(id.clone());
        })?;
        Ok(ConversationSummary {
            id,
            title,
            model,
            mode: Mode::Build,
            created_at,
            turns: 0,
            busy: false,
        })
    }

    pub fn conversation_select(&self, id: &str) -> ServiceResult<()> {
        self.conversation_meta(id)?;
        self.update_meta(|m| m.active_conversation = Some(id.to_string()))
    }

    pub fn conversation_transcript(
        &self,
        id: &str,
    ) -> ServiceResult<Vec<analyst_protocol::transcript::Event>> {
        self.conversation_meta(id)?;
        Ok(self.session_snapshot().read_transcript(id)?)
    }

    pub async fn chat_set_mode(&self, id: &str, mode: Mode) -> ServiceResult<()> {
        self.conversation_meta(id)?;
        self.update_meta(|m| {
            if let Some(c) = m.conversations.iter_mut().find(|c| c.id == id) {
                c.mode = mode;
            }
        })?;
        if let Some(c) = self.conversation(id) {
            c.set_mode(mode).await;
        }
        self.emit_session_changed().await;
        Ok(())
    }

    pub async fn chat_set_model(&self, id: &str, model: &str) -> ServiceResult<()> {
        self.conversation_meta(id)?;
        let spec =
            config::ModelSpec::parse(model).map_err(|e| ServiceError::invalid(e.to_string()))?;
        config::resolve_model(&self.global(), &spec)
            .map_err(|e| ServiceError::not_found(e.to_string()))?;
        self.update_meta(|m| {
            if let Some(c) = m.conversations.iter_mut().find(|c| c.id == id) {
                c.model = Some(model.to_string());
            }
        })?;
        if let Some(c) = self.conversation(id) {
            c.invalidate_model();
        }
        self.emit_session_changed().await;
        Ok(())
    }

    /// Resolve the pending proposal; `not_found` when it was already resolved.
    pub fn proposal_respond(
        &self,
        proposal_id: &str,
        decision: ProposalDecision,
    ) -> ServiceResult<()> {
        let pending = self
            .pending
            .lock()
            .ok()
            .and_then(|mut p| p.remove(proposal_id))
            .ok_or_else(|| {
                ServiceError::not_found(format!("proposal `{proposal_id}` is not pending"))
            })?;
        pending.tx.send(decision).map_err(|_| {
            ServiceError::not_found(format!("proposal `{proposal_id}` is no longer awaited"))
        })
    }

    /// Every conversation rebuilds its model client before its next turn (a provider key
    /// changed).
    pub fn invalidate_models(&self) {
        let convs: Vec<Arc<ConversationRuntime>> = self
            .conversations
            .lock()
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default();
        for c in convs {
            c.invalidate_model();
        }
    }

    /// Reject every pending proposal of a conversation (`chat_cancel`, close).
    pub fn reject_pending(&self, conversation: Option<&str>) {
        let Ok(mut p) = self.pending.lock() else {
            return;
        };
        let ids: Vec<String> = p
            .iter()
            .filter(|(_, v)| conversation.map(|c| v.conversation == c).unwrap_or(true))
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if let Some(pp) = p.remove(&id) {
                let _ = pp.tx.send(ProposalDecision::Reject {
                    reason: Some("cancelled".into()),
                });
                self.emit(SessionEvent::ProposalResolved {
                    proposal_id: id,
                    decision: analyst_protocol::transcript::Decision::Reject,
                    by: analyst_protocol::transcript::DecidedBy::User,
                });
            }
        }
    }

    // ---- watcher ----

    /// Record (once) and report that file watching is not working, as a failed task with a
    /// synthetic id so the renderer shows it (U2). New subscribers are told again.
    fn report_watcher_failure(&self, message: String) {
        tracing::warn!("{message}");
        let first = match self.watcher_error.lock() {
            Ok(mut e) if e.is_none() => {
                *e = Some(message.clone());
                true
            }
            _ => false,
        };
        if first {
            let task_id = format!("task-watcher-{}", self.id);
            self.emit(SessionEvent::TaskLog {
                task_id: task_id.clone(),
                line: message.clone(),
            });
            self.emit(SessionEvent::TaskDone {
                task_id,
                ok: false,
                message: Some(message),
            });
        }
    }

    /// The directories to watch below `start` (inclusive): every directory except `.git`,
    /// `.venv`, `node_modules`, `__pycache__`, and tool caches. Inside the output directory
    /// only `out/` and `out/<script>/` are covered; run directories are added when they
    /// appear (see [`Self::watch_new_dirs`]), so a long run history costs no watches.
    fn watch_dirs_under(&self, start: &Path) -> Vec<PathBuf> {
        let out_abs = self.repo.join(self.out_dir().trim_matches('/'));
        let mut dirs = Vec::new();
        let mut stack = vec![start.to_path_buf()];
        while let Some(dir) = stack.pop() {
            // Symlinked directories are not followed.
            let Ok(meta) = std::fs::symlink_metadata(&dir) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let depth_in_out = dir
                .strip_prefix(&out_abs)
                .ok()
                .map(|r| r.components().count());
            dirs.push(dir.clone());
            if matches!(depth_in_out, Some(d) if d >= 1) {
                continue; // out/<script>: its run directories are added on demand
            }
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if WATCH_SKIP.contains(&name.as_str()) {
                    continue;
                }
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    stack.push(e.path());
                }
            }
        }
        dirs
    }

    /// Add non-recursive watches. Returns the first error (for example the inotify watch
    /// limit), after which no further directory is tried.
    fn add_watches(&self, dirs: Vec<PathBuf>) -> Result<(), String> {
        use notify::Watcher;
        let Ok(mut guard) = self.watcher.lock() else {
            return Ok(());
        };
        let Some(ws) = guard.as_mut() else {
            return Ok(());
        };
        for d in dirs {
            if ws.watched.contains(&d) {
                continue;
            }
            match ws.watcher.watch(&d, notify::RecursiveMode::NonRecursive) {
                Ok(()) => {
                    ws.watched.insert(d);
                }
                // The directory vanished between the listing and the watch.
                Err(e) if matches!(e.kind, notify::ErrorKind::PathNotFound) => {}
                Err(e) if matches!(&e.kind, notify::ErrorKind::Io(io) if io.kind() == std::io::ErrorKind::NotFound) =>
                    {}
                Err(e) => {
                    return Err(format!(
                        "File watching is incomplete ({} directories watched, then: {e}). Changes made outside the app may not show up until you refresh; on Linux, raising fs.inotify.max_user_watches fixes this.",
                        ws.watched.len()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Watch directories that appeared in a batch of events: new source directories with
    /// everything below them, and new run directories (`out/<script>/<run>`) with their
    /// `figures/` child so terminal figures are noticed.
    fn watch_new_dirs(&self, batch: &[PathBuf]) {
        let out_abs = self.repo.join(self.out_dir().trim_matches('/'));
        let mut dirs: Vec<PathBuf> = Vec::new();
        for p in batch {
            let Ok(meta) = std::fs::symlink_metadata(p) else {
                continue;
            };
            if !meta.is_dir() || self.rel_of(p).is_none() {
                continue;
            }
            match p
                .strip_prefix(&out_abs)
                .ok()
                .map(|r| r.components().count())
            {
                None | Some(0) => dirs.extend(self.watch_dirs_under(p)),
                Some(1) => {
                    // out/<script> appeared: watch it and the run directories already in it.
                    dirs.push(p.clone());
                    if let Ok(rd) = std::fs::read_dir(p) {
                        for e in rd.flatten() {
                            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                dirs.push(e.path());
                                dirs.push(e.path().join("figures"));
                            }
                        }
                    }
                }
                Some(2) => {
                    dirs.push(p.clone());
                    dirs.push(p.join("figures"));
                }
                Some(3) if p.file_name().map(|n| n == "figures").unwrap_or(false) => {
                    dirs.push(p.clone())
                }
                Some(_) => {}
            }
        }
        dirs.retain(|d| d.is_dir());
        if dirs.is_empty() {
            return;
        }
        if let Err(m) = self.add_watches(dirs) {
            self.report_watcher_failure(m);
        }
    }

    fn start_watcher(&self) {
        let Some(rt) = self.arc() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<PathBuf>>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    return;
                }
                let _ = tx.send(ev.paths);
            }
        });
        let watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                self.report_watcher_failure(format!(
                    "File watching is unavailable ({e}). Changes made outside the app will not show up until you refresh."
                ));
                return;
            }
        };
        if let Ok(mut w) = self.watcher.lock() {
            *w = Some(WatchState {
                watcher,
                watched: Default::default(),
            });
        }
        // One watch per directory instead of one recursive watch: `.git`, `.venv`,
        // `node_modules`, and the run history under `out/` are never descended into (U2).
        if let Err(m) = self.add_watches(self.watch_dirs_under(&self.repo)) {
            self.report_watcher_failure(m);
        }
        self.spawn(async move {
            while let Some(first) = rx.recv().await {
                let mut batch: Vec<PathBuf> = first;
                let deadline = tokio::time::Instant::now() + DEBOUNCE;
                loop {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Some(more)) => batch.extend(more),
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }
                if rt.is_closed() {
                    return;
                }
                rt.watch_new_dirs(&batch);
                rt.handle_fs_batch(batch).await;
            }
        });
    }

    /// Number of directories the watcher covers (tests and diagnostics).
    pub fn watched_dirs(&self) -> usize {
        self.watcher
            .lock()
            .ok()
            .and_then(|w| w.as_ref().map(|w| w.watched.len()))
            .unwrap_or(0)
    }

    fn rel_of(&self, p: &Path) -> Option<String> {
        let rel = p.strip_prefix(&self.repo).ok()?;
        let s = rel.to_string_lossy().replace('\\', "/");
        if s.is_empty() {
            return None;
        }
        if s.split('/').any(|c| WATCH_SKIP.contains(&c)) || s.ends_with(".tmp") || s.ends_with('~')
        {
            return None;
        }
        Some(s)
    }

    async fn handle_fs_batch(&self, batch: Vec<PathBuf>) {
        let mut paths: BTreeSet<String> = BTreeSet::new();
        for p in &batch {
            if let Some(r) = self.rel_of(p) {
                paths.insert(r);
            }
        }
        if paths.is_empty() {
            return;
        }
        let out_dir = self.out_dir();
        let out_prefix = format!("{}/", out_dir.trim_matches('/'));
        let paths: Vec<String> = paths.into_iter().collect();
        self.emit(SessionEvent::FilesChanged {
            paths: paths.clone(),
        });
        if paths.iter().any(|p| !p.starts_with(&out_prefix)) {
            self.mirror_dirty.store(true, Ordering::SeqCst);
        }
        let py_changed = paths
            .iter()
            .any(|p| p.ends_with(".py") && !p.starts_with(&out_prefix));
        if py_changed && self.kernel_client().await.is_some() {
            let _ = self.refresh_index().await;
        }
        let mut run_dirs: BTreeSet<String> = BTreeSet::new();
        for p in &paths {
            if let Some(d) = figures::run_dir_of(&out_dir, p) {
                run_dirs.insert(d);
            } else if let Some(rest) = p.strip_prefix(&out_prefix) {
                // `out/<script>` itself appeared (new directory tree): inotify may have missed
                // what was created inside it before the watch was added, so look now.
                if !rest.contains('/') && !rest.starts_with('_') {
                    if let Ok(rd) = std::fs::read_dir(self.repo.join(p)) {
                        for e in rd.flatten() {
                            if e.path().is_dir() {
                                let name = e.file_name().to_string_lossy().to_string();
                                if !name.starts_with('_') {
                                    run_dirs.insert(format!("{p}/{name}"));
                                }
                            }
                        }
                    }
                }
            }
        }
        for d in run_dirs {
            self.discover_run_dir(&d).await;
        }
    }

    /// A run directory under `out/` changed: register terminal runs (run_started + run_done)
    /// and report their figures as artifacts.
    async fn discover_run_dir(&self, dir_rel: &str) {
        // Never follow a symlinked run directory out of the repo.
        if resolve_in_repo(&self.repo, dir_rel, true).is_err() {
            return;
        }
        let abs = self.repo.join(dir_rel);
        if crate::util::is_symlink(&abs) || !abs.is_dir() {
            return;
        }
        // Terminals opened by the app get a run.json once they produce something.
        let run_json = abs.join("run.json");
        if !run_json.is_file() {
            let parent = Path::new(dir_rel)
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if parent == "terminal" {
                let stamp = Path::new(dir_rel)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let j = serde_json::json!({
                    "run_id": format!("terminal-{stamp}"),
                    "script": "terminal",
                    "args": [],
                    "started_at": now_rfc3339(),
                    "host": "local",
                    "tree_hash": null,
                });
                let _ = std::fs::write(&run_json, j.to_string());
            }
        }
        let Some(scanned) = figures::scan_run_dir(&self.repo, &abs) else {
            return;
        };
        if !scanned.has_run_json && scanned.figures.is_empty() {
            return;
        }
        let existing = self.run_record(&scanned.run_id);
        let record = match existing {
            Some(r) => r,
            None => {
                // A kernel run started elsewhere (agent tool) shows up here too.
                if let Some(r) = self.register_kernel_run(&scanned.run_id).await {
                    r
                } else {
                    let view = self.synthesized_view(&scanned);
                    let rec = RunRecord {
                        meta: view.meta.clone(),
                        target: view.target.clone(),
                        stdout: String::new(),
                        stderr: String::new(),
                        artifacts: vec![],
                        source: RunSource::Terminal,
                        finished: true,
                        conversation: None,
                    };
                    self.insert_run(rec.clone());
                    if let Ok(s) = self.session.lock() {
                        let _ = s.append_run(&rec.meta);
                    }
                    self.emit(SessionEvent::RunStarted { run: rec.view() });
                    self.emit(SessionEvent::RunDone { run: rec.view() });
                    rec
                }
            }
        };
        if record.source != RunSource::Terminal {
            return;
        }
        for f in scanned.figures {
            let known = record.artifacts.iter().any(|a| a.path == f.path);
            if known {
                continue;
            }
            let artifact = Artifact {
                kind: ArtifactKind::Figure,
                path: f.path.clone(),
                size: f.size,
                created_at: f.created_at.clone(),
                sidecar: f.sidecar.clone(),
                rendered: true,
            };
            self.with_run(&scanned.run_id, |r| r.artifacts.push(artifact.clone()));
            self.emit(SessionEvent::RunArtifact {
                run_id: scanned.run_id.clone(),
                artifact,
                figure: Some(f),
            });
        }
    }

    // ---- lifecycle ----

    /// Suspend the session: cancel turns, reject proposals, stop the watcher and the kernel.
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let convs: Vec<Arc<ConversationRuntime>> = self
            .conversations
            .lock()
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default();
        let aborted: Vec<JoinHandle<()>> = convs.iter().filter_map(|c| c.abort()).collect();
        for h in aborted {
            // The turn's guard rejects its proposals and its runs are cancelled on drop.
            let _ = tokio::time::timeout(Duration::from_secs(5), h).await;
        }
        self.reject_pending(None);
        if let Ok(mut w) = self.watcher.lock() {
            *w = None;
        }
        self.shutdown_kernel().await;
        let handles: Vec<JoinHandle<()>> = self
            .background
            .lock()
            .map(|mut b| b.drain(..).collect())
            .unwrap_or_default();
        for h in handles {
            h.abort();
        }
    }
}

pub fn guess_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" | "txt" | "log" | "py" | "toml" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Directory names the watcher never enters and whose events are ignored.
const WATCH_SKIP: &[&str] = &[
    ".git",
    ".venv",
    "node_modules",
    "__pycache__",
    ".ruff_cache",
    ".pytest_cache",
    ".mypy_cache",
];

/// The tooling interpreter for a global config and analyst home (dev checkout aware).
pub fn tooling_python(global: &GlobalConfig, home: &Path) -> String {
    resolve_tooling_python_in(global, home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_run_output_is_bounded() {
        let mut buf = String::new();
        for i in 0..20_000 {
            push_bounded(&mut buf, &format!("line {i} é\n"));
        }
        assert!(buf.len() <= RUN_MEMORY_BYTES * 2);
        assert!(buf.ends_with("line 19999 é\n"));
        assert!(tail(&buf, TAIL_BYTES).len() <= TAIL_BYTES);
    }

    #[test]
    fn log_tail_is_capped_with_a_note() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("stdout.log");
        assert_eq!(read_log_tail(&p, 100), None);
        std::fs::write(&p, "short\n").unwrap();
        assert_eq!(read_log_tail(&p, 100).as_deref(), Some("short\n"));
        std::fs::write(&p, format!("{}é-end", "ab".repeat(500))).unwrap();
        let t = read_log_tail(&p, 6).unwrap();
        assert!(t.starts_with("[... output truncated: the first 1000 bytes"));
        assert!(t.ends_with("é-end"), "{t:?}");
        // A cut in the middle of a multi-byte character does not produce garbage.
        let t = read_log_tail(&p, 5).unwrap();
        assert!(t.ends_with("\n-end"), "{t:?}");
    }

    #[test]
    fn tooling_wait_follows_the_state() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let t = Tooling::ready();
            assert_eq!(t.wait().await, Ok(()));
            t.set(ToolingState::Preparing);
            let t2 = t.clone();
            let w = tokio::spawn(async move { t2.wait().await });
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(!w.is_finished());
            t.set(ToolingState::Failed("no uv".into()));
            assert_eq!(w.await.unwrap(), Err("no uv".to_string()));
        });
    }
}
