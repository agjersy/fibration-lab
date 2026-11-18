//! The run service: the one owner of runs in a session (DESIGN-REVIEW item 1).
//!
//! The agent's `run.script` tool and the UI's `run_start` command both call
//! [`RunService::start`]; the kernel's notifications for every run arrive in
//! [`RunService::handle_notification`], which keeps the run records, writes the run history,
//! pulls rendered artifacts from a remote target once, and emits the `run_*` session events.
//! A run is a run, whoever started it: there is no second subscriber to reconcile with.
//!
//! The service also owns the mirror sync state of a remote target, because a run is what
//! needs the mirror to be current.

use super::error::{ServiceError, ServiceResult};
use super::events::EventHub;
use super::figures;
use crate::config;
use crate::kernel::KernelClient;
use crate::session::Session;
use crate::target::{self, Target};
use crate::util::{normalize_rel_path, now_rfc3339, resolve_in_repo};
use analyst_protocol::config::ProjectConfig;
use analyst_protocol::kernel::*;
use analyst_protocol::ui::*;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Last N bytes of stdout/stderr carried in a `RunView`.
pub const TAIL_BYTES: usize = 4096;
/// Bytes of each stream kept in memory per run (the full text is in the run's log files).
pub const RUN_MEMORY_BYTES: usize = 64 * 1024;
/// `run_get` returns at most this much of each stream (the tail), with a note when cut.
pub const RUN_GET_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Finished runs kept in memory; older ones are served from the session store.
pub const MAX_FINISHED_RUNS: usize = 200;
/// Minimum time between mirror syncs triggered by index, check, and equation requests (R4).
pub const SYNC_DEBOUNCE: Duration = Duration::from_secs(2);

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
    /// The kernel reported the end of the run.
    pub finished: bool,
    /// `run_done` was emitted: the run finished and, on a remote target, its rendered
    /// artifacts were pulled (or the pull failed, see `pull_error`).
    pub completed: bool,
    /// The conversation whose agent turn started the run (IPC.md section 7).
    pub conversation: Option<String>,
    pub duration_s: Option<f64>,
    /// Why the rendered artifacts of a remote run could not be pulled, if they could not.
    pub pull_error: Option<String>,
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
pub(super) fn push_bounded(buf: &mut String, chunk: &str) {
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
pub(super) fn read_log_tail(path: &Path, max: u64) -> Option<String> {
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

/// Session state the run service shares with whoever owns the session (the desktop
/// `SessionRuntime`, or a CLI command): the kernel connection, the target, the project
/// config, and the session record.
#[derive(Clone)]
pub struct RunShared {
    pub session: Arc<Mutex<Session>>,
    pub target: Arc<RwLock<Target>>,
    pub project: Arc<RwLock<ProjectConfig>>,
    pub kernel: Arc<RwLock<Option<KernelClient>>>,
}

impl RunShared {
    pub fn new(session: Session, target: Target, project: ProjectConfig) -> RunShared {
        RunShared {
            session: Arc::new(Mutex::new(session)),
            target: Arc::new(RwLock::new(target)),
            project: Arc::new(RwLock::new(project)),
            kernel: Arc::new(RwLock::new(None)),
        }
    }
}

pub struct RunService {
    repo: PathBuf,
    home: PathBuf,
    /// The session directory (`~/.analyst/sessions/<id>`).
    dir: PathBuf,
    control_dir: RwLock<Option<PathBuf>>,
    shared: RunShared,
    events: EventHub,
    runs: Mutex<HashMap<String, RunRecord>>,
    /// run id -> conversation, from the transcripts on disk plus the runs of this process.
    run_conversations: Mutex<HashMap<String, String>>,
    run_conversations_loaded: AtomicBool,
    registration: tokio::sync::Mutex<()>,
    /// Woken whenever a run record changes state.
    changed: Notify,
    last_sync_hash: tokio::sync::Mutex<Option<String>>,
    /// Local files changed since the last mirror sync.
    mirror_dirty: AtomicBool,
    last_mirror_sync: Mutex<Option<std::time::Instant>>,
    /// Forwarder of a kernel attached with [`RunService::attach_kernel`].
    forwarder: Mutex<Option<JoinHandle<()>>>,
    generation: AtomicU64,
    this: Weak<RunService>,
}

impl RunService {
    pub fn new(
        repo: &Path,
        home: &Path,
        dir: &Path,
        control_dir: Option<PathBuf>,
        shared: RunShared,
        events: EventHub,
    ) -> Arc<RunService> {
        Arc::new_cyclic(|weak| RunService {
            repo: repo.to_path_buf(),
            home: home.to_path_buf(),
            dir: dir.to_path_buf(),
            control_dir: RwLock::new(control_dir),
            shared,
            events,
            runs: Mutex::new(HashMap::new()),
            run_conversations: Mutex::new(HashMap::new()),
            run_conversations_loaded: AtomicBool::new(false),
            registration: tokio::sync::Mutex::new(()),
            changed: Notify::new(),
            last_sync_hash: tokio::sync::Mutex::new(None),
            mirror_dirty: AtomicBool::new(true),
            last_mirror_sync: Mutex::new(None),
            forwarder: Mutex::new(None),
            generation: AtomicU64::new(0),
            this: weak.clone(),
        })
    }

    /// A run service that is not part of a desktop session runtime (CLI commands,
    /// evaluations, tests). Attach a kernel with [`RunService::attach_kernel`].
    pub fn standalone(
        session: &Session,
        target: Target,
        project: ProjectConfig,
    ) -> Arc<RunService> {
        RunService::new(
            &session.repo(),
            &session.home,
            &session.dir,
            None,
            RunShared::new(session.clone(), target, project),
            EventHub::new(),
        )
    }

    pub fn hub(&self) -> EventHub {
        self.events.clone()
    }

    fn emit(&self, e: SessionEvent) {
        self.events.emit(e);
    }

    pub fn target(&self) -> Target {
        self.shared
            .target
            .read()
            .map(|t| t.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn set_target(&self, target: Target) {
        if let Ok(mut t) = self.shared.target.write() {
            *t = target;
        }
    }

    pub fn project(&self) -> ProjectConfig {
        self.shared
            .project
            .read()
            .map(|p| p.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn set_project(&self, project: ProjectConfig) {
        if let Ok(mut p) = self.shared.project.write() {
            *p = project;
        }
    }

    pub fn set_control_dir(&self, dir: Option<PathBuf>) {
        if let Ok(mut c) = self.control_dir.write() {
            *c = dir;
        }
    }

    fn control_dir(&self) -> Option<PathBuf> {
        self.control_dir.read().ok().and_then(|c| c.clone())
    }

    fn session(&self) -> Session {
        self.shared
            .session
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    /// The connected kernel client, if any.
    pub fn kernel(&self) -> Option<KernelClient> {
        self.shared.kernel.read().ok().and_then(|k| k.clone())
    }

    /// Use `kernel` for runs and forward its notifications into this service. For owners
    /// without their own forwarder (everything but the desktop session runtime, which
    /// watches the connection itself and calls [`RunService::handle_notification`]).
    pub fn attach_kernel(&self, kernel: Option<KernelClient>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut k) = self.shared.kernel.write() {
            *k = kernel.clone();
        }
        let handle = match (kernel, self.this.upgrade()) {
            (Some(k), Some(svc)) => {
                let mut rx = k.subscribe();
                let weak = Arc::downgrade(&svc);
                drop(svc);
                Some(tokio::spawn(async move {
                    loop {
                        let n = match rx.recv().await {
                            Ok(n) => n,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        };
                        let Some(svc) = weak.upgrade() else { break };
                        if svc.generation.load(Ordering::SeqCst) != generation {
                            break;
                        }
                        svc.handle_notification(n).await;
                    }
                }))
            }
            _ => None,
        };
        if let Ok(mut f) = self.forwarder.lock() {
            if let Some(old) = std::mem::replace(&mut *f, handle) {
                old.abort();
            }
        }
    }

    // ---- records ----

    pub fn record(&self, run_id: &str) -> Option<RunRecord> {
        self.runs.lock().ok().and_then(|r| r.get(run_id).cloned())
    }

    pub fn with_run<R>(&self, run_id: &str, f: impl FnOnce(&mut RunRecord) -> R) -> Option<R> {
        let r = self
            .runs
            .lock()
            .ok()
            .and_then(|mut r| r.get_mut(run_id).map(f));
        self.changed.notify_waiters();
        r
    }

    /// Insert a record and evict the oldest finished ones beyond [`MAX_FINISHED_RUNS`]; they
    /// stay available through the session store (`list`, `get`).
    pub fn insert(&self, rec: RunRecord) {
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
        drop(r);
        self.changed.notify_waiters();
    }

    /// Number of run records held in memory.
    pub fn in_memory(&self) -> usize {
        self.runs.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Record a run that did not come from the kernel (a terminal run found on disk) and
    /// announce it.
    pub fn record_external(&self, rec: RunRecord) {
        self.insert(rec.clone());
        let _ = self.session().append_run(&rec.meta);
        self.emit(SessionEvent::RunStarted { run: rec.view() });
        self.emit(SessionEvent::RunDone { run: rec.view() });
    }

    /// run id -> conversation for every agent run, read once from the transcripts.
    fn load_run_conversations(&self) {
        if self.run_conversations_loaded.swap(true, Ordering::SeqCst) {
            return;
        }
        let session = self.session();
        let mut found: Vec<(String, String)> = Vec::new();
        for c in session.conversations().unwrap_or_default() {
            if let Ok(events) = session.read_transcript(&c) {
                for e in events {
                    if let analyst_protocol::transcript::Event::Run { run_id, .. } = e {
                        found.push((run_id, c.clone()));
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
            completed: finished,
            conversation,
            duration_s: None,
            pull_error: None,
        }
    }

    /// A `RunView` for a run known only from the session store.
    pub fn stored_view(&self, r: analyst_protocol::session::SessionRun) -> RunView {
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

    /// Register a kernel run this service did not start itself (a terminal run through the
    /// kernel, a run of an earlier process) when its first notification or `run.json` shows
    /// up.
    pub async fn register_kernel_run(&self, run_id: &str) -> Option<RunRecord> {
        let _g = self.registration.lock().await;
        if let Some(r) = self.record(run_id) {
            return Some(r);
        }
        let k = self.kernel()?;
        let meta = k.run_status(run_id).await.ok()?;
        if meta.run_id != run_id {
            return None;
        }
        let rec = self.record_from_meta(meta, RunSource::Kernel);
        self.insert(rec.clone());
        let _ = self.session().append_run(&rec.meta);
        self.emit(SessionEvent::RunStarted { run: rec.view() });
        if rec.finished {
            self.emit(SessionEvent::RunDone { run: rec.view() });
        }
        Some(rec)
    }

    // ---- start, wait, cancel ----

    /// Start a run of a repo script on the target. `conversation` is the agent conversation
    /// that asked for it, if one did. Needs a connected kernel; on a remote target the
    /// mirror is synced first. Emits `run_started`; output, artifacts, and `run_done` follow
    /// from the kernel's notifications.
    pub async fn start(
        &self,
        script: &str,
        args: Vec<String>,
        timeout_s: Option<f64>,
        conversation: Option<&str>,
    ) -> ServiceResult<RunView> {
        let script = normalize_rel_path(script).map_err(ServiceError::invalid)?;
        let script_abs =
            resolve_in_repo(&self.repo, &script, true).map_err(ServiceError::not_found)?;
        if !script_abs.is_file() {
            return Err(ServiceError::not_found(format!(
                "script `{script}` not found"
            )));
        }
        let k = self.kernel().ok_or_else(|| {
            ServiceError::kernel_offline(
                "the kernel is not connected on this target (target offline)",
            )
        })?;
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
        let _g = self.registration.lock().await;
        let started = k
            .run_start(RunStartParams {
                script: script.clone(),
                args: Some(args.clone()),
                env: None,
                timeout_s: Some(timeout_s.unwrap_or(eff.timeout_s as f64)),
                tree_hash,
            })
            .await?;
        if !figures::safe_run_id(&started.run_id) {
            return Err(ServiceError::internal(
                "the kernel returned an unusable run id",
            ));
        }
        if let Some(c) = conversation {
            if let Ok(mut m) = self.run_conversations.lock() {
                m.insert(started.run_id.clone(), c.to_string());
            }
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
        let mut rec = self.record_from_meta(meta, RunSource::Kernel);
        // The end of the run is reported by its `run.done` notification, also when the run
        // is already over by the time its status was read.
        rec.finished = false;
        rec.completed = false;
        self.insert(rec.clone());
        let _ = self.session().append_run(&rec.meta);
        let view = rec.view();
        self.emit(SessionEvent::RunStarted { run: view.clone() });
        Ok(view)
    }

    /// Wait until the run is over (its `run_done` was emitted) or `wait` elapsed, and return
    /// the record as it is then. `None` for an unknown run.
    pub async fn wait(&self, run_id: &str, wait: Duration) -> Option<RunRecord> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let rec = self.record(run_id)?;
            if rec.completed {
                return Some(rec);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.record(run_id);
            }
        }
    }

    pub async fn cancel(&self, run_id: &str) -> ServiceResult<()> {
        let k = self
            .kernel()
            .ok_or_else(|| ServiceError::kernel_offline("the kernel is not connected"))?;
        let r = k.run_cancel(run_id).await?;
        if !r.ok {
            return Err(ServiceError::not_found(format!(
                "run `{run_id}` is not running"
            )));
        }
        Ok(())
    }

    // ---- kernel notifications ----

    pub async fn handle_notification(&self, n: KernelNotification) {
        let Some(run_id) = n.run_id().map(|s| s.to_string()) else {
            return;
        };
        // Run ids become path components (session store, pulled artifacts).
        if !figures::safe_run_id(&run_id) {
            tracing::warn!("ignoring a kernel notification with an unusable run id");
            return;
        }
        if self.record(&run_id).is_none() && self.register_kernel_run(&run_id).await.is_none() {
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
                let meta = match self.kernel() {
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
                    r.duration_s = Some(d.duration_s);
                    r.view()
                });
                let Some(v) = view else { return };
                let _ = self.session().append_run(&v.meta);
                if !remote {
                    self.complete(v, None);
                    return;
                }
                // Remote: the kernel's run record is on the host, so the artifact list is
                // kept with the session.
                let store = self.run_store_dir(&v.meta.run_id);
                let _ = std::fs::create_dir_all(&store);
                let lines: String = v
                    .artifacts
                    .iter()
                    .filter_map(|a| serde_json::to_string(a).ok())
                    .map(|l| l + "\n")
                    .collect();
                let _ = std::fs::write(store.join("artifacts.jsonl"), lines);
                // Pull the rendered artifacts once, off this task (G8), whoever started
                // the run.
                if let Some(svc) = self.this.upgrade() {
                    tokio::spawn(async move {
                        let err = svc.pull_run_artifacts(&v).await.err();
                        svc.complete(v, err);
                    });
                }
            }
            KernelNotification::Other { .. } => {}
        }
    }

    fn complete(&self, view: RunView, pull_error: Option<String>) {
        self.with_run(&view.meta.run_id, |r| {
            r.completed = true;
            r.pull_error = pull_error;
        });
        self.emit(SessionEvent::RunDone { run: view });
    }

    /// On a remote target, pull the run's rendered artifacts (figures, their sidecars,
    /// `run.json`; nothing else leaves the host) into the repo and the session.
    async fn pull_run_artifacts(&self, v: &RunView) -> Result<(), String> {
        let target = self.target();
        let (Target::Ssh { host, .. }, Some(mirror)) = (&target, target.mirror_dir(&self.repo))
        else {
            return Ok(());
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
            self.control_dir().as_deref(),
        )
        .await
        {
            tracing::warn!("pulling run {} failed: {e}", v.meta.run_id);
            return Err(e.to_string());
        }
        let session = self.session();
        for rel in &paths {
            let Ok(src) = resolve_in_repo(&self.repo, rel, true) else {
                continue;
            };
            if !src.is_file() {
                continue;
            }
            let dst = session.pulled_artifact_path(&v.meta.run_id, rel);
            if let Some(p) = dst.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let _ = std::fs::copy(&src, &dst);
        }
        Ok(())
    }

    // ---- history ----

    /// Runs known to this process plus the session's run history, newest first. Needs no
    /// kernel.
    pub fn list(&self, limit: Option<u32>) -> Vec<RunView> {
        let mut views: Vec<RunView> = self
            .runs
            .lock()
            .map(|r| r.values().map(|v| v.view()).collect())
            .unwrap_or_default();
        let mut seen: BTreeSet<String> = views.iter().map(|v| v.meta.run_id.clone()).collect();
        if let Ok(runs) = self.session().read_runs() {
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

    /// The artifacts the kernel reported for a run: from memory, else from the artifact list
    /// kept with the session (or, for a local run, with the kernel's run record).
    pub fn artifacts(&self, run_id: &str) -> Vec<Artifact> {
        if !figures::safe_run_id(run_id) {
            return vec![];
        }
        if let Some(r) = self.record(run_id) {
            if !r.artifacts.is_empty() {
                return r.artifacts;
            }
        }
        [
            self.home.join("runs").join(run_id),
            self.run_store_dir(run_id),
        ]
        .iter()
        .find_map(|d| {
            crate::session::read_jsonl::<Artifact>(&d.join("artifacts.jsonl"))
                .ok()
                .filter(|a| !a.is_empty())
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|a| normalize_rel_path(&a.path).is_ok())
        .collect()
    }

    /// A run with its output: any run `list` shows, also after a restart. The text comes
    /// from the run's log files (the kernel's for local runs, the session store for remote
    /// ones); each stream is cut to its last [`RUN_GET_MAX_BYTES`] with a note. Needs no
    /// kernel.
    pub fn get(&self, run_id: &str) -> ServiceResult<RunDetail> {
        if !figures::safe_run_id(run_id) {
            return Err(ServiceError::not_found(format!("run `{run_id}` not found")));
        }
        let record = self.record(run_id);
        let mut view = match &record {
            Some(r) => r.view(),
            None => {
                let run = self
                    .session()
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
            view.artifacts = self.artifacts(run_id);
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

    // ---- mirror sync ----

    /// A local file changed: the mirror of a remote target is out of date.
    pub fn mark_dirty(&self) {
        self.mirror_dirty.store(true, Ordering::SeqCst);
    }

    /// Forget what was synced (the target changed).
    pub async fn reset_sync(&self) {
        *self.last_sync_hash.lock().await = None;
        self.mirror_dirty.store(true, Ordering::SeqCst);
        if let Ok(mut t) = self.last_mirror_sync.lock() {
            *t = None;
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
            self.control_dir().as_deref(),
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
        if let Ok(mut s) = self.shared.session.lock() {
            let _ = s.set_last_tree_hash(Some(report.tree_hash.clone()));
        }
        Ok(Some(report.tree_hash))
    }

    /// Bring the mirror up to date before a request that reads it (index, checks, equations)
    /// on a remote target (R4). Nothing happens when no local file changed since the last
    /// sync (unless the file watcher is broken and cannot tell); syncs are at least
    /// [`SYNC_DEBOUNCE`] apart.
    pub async fn sync_debounced(&self, watcher_broken: bool) -> ServiceResult<()> {
        if !self.target().is_remote() {
            return Ok(());
        }
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
}

impl Drop for RunService {
    fn drop(&mut self) {
        if let Ok(mut f) = self.forwarder.lock() {
            if let Some(h) = f.take() {
                h.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_run_output_is_bounded() {
        let mut buf = String::new();
        let chunk = "x".repeat(10_000);
        for _ in 0..100 {
            push_bounded(&mut buf, &chunk);
        }
        assert!(buf.len() <= RUN_MEMORY_BYTES * 2);
        assert!(buf.len() >= RUN_MEMORY_BYTES);
    }

    #[test]
    fn log_tail_is_capped_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("stdout.log");
        std::fs::write(&p, "a".repeat(1000) + "TAIL").unwrap();
        let whole = read_log_tail(&p, 10_000).unwrap();
        assert_eq!(whole.len(), 1004);
        assert!(!whole.contains("truncated"));
        let cut = read_log_tail(&p, 100).unwrap();
        assert!(cut.starts_with("[... output truncated: the first 904 bytes are not shown ...]\n"));
        assert!(cut.ends_with("TAIL"));
        assert!(read_log_tail(&dir.path().join("missing.log"), 100).is_none());
    }
}
