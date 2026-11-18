//! The desktop service layer (docs/IPC.md): a [`Workbench`] per app that owns the global config,
//! the open [`SessionRuntime`]s, terminals, and task ids. The Tauri commands in
//! `apps/desktop/src-tauri` are thin wrappers over these methods; the service never touches the
//! webview.

pub mod agent_runner;
pub mod artifacts;
pub mod decks;
pub mod error;
pub mod events;
pub mod figures;
pub mod fs;
pub mod history;
pub mod packages;
pub mod provenance;
pub mod refresh;
pub mod runs;
pub mod runtime;
pub mod tasks;
pub mod terminal;
pub mod verification;

pub use agent_runner::{ConversationRuntime, UiApprover};
pub use decks::{DeckOptions, DeckServices};
pub use error::{ServiceError, ServiceResult};
pub use events::{EventHub, Subscription};
pub use runs::{RunRecord, RunService, RunShared};
pub use runtime::{RuntimeOptions, SessionRuntime, Tooling, ToolingState};
pub use tasks::{TaskLogger, TaskRegistry};
pub use terminal::{CaptureEnv, Terminal, TerminalManager, TerminalSpec};

use crate::config;
use crate::session::Session;
use crate::target::Target;
use analyst_protocol::config::{GlobalConfig, ProviderKind, TargetKind};
use analyst_protocol::ui::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use wb_providers::capabilities;

/// Options for the whole app.
#[derive(Debug, Clone)]
pub struct WorkbenchOptions {
    /// `$ANALYST_HOME` (default `~/.analyst`).
    pub home: PathBuf,
    /// Override of the local kernel launcher (tests inject a fake kernel).
    pub kernel_launcher: Option<Vec<String>>,
    /// `--sandbox` override for kernels.
    pub sandbox: Option<String>,
    /// Session interpreter override.
    pub session_python: Option<String>,
    /// Start the kernel in the background on `session_open`.
    pub autostart_kernel: bool,
    /// Reported in `app_info`.
    pub dev_mode: bool,
    /// How `session_create { init: true }` applies the template.
    pub init: crate::init::InitOptions,
    /// The kernel source bundled with a packaged app (a directory with `pyproject.toml`,
    /// `analyst_kernel/`, and `analyst_mpl/`). See [`Workbench::prepare_tooling`].
    pub kernel_source: Option<PathBuf>,
    /// How deck kernels are started (engine, sandbox, interpreter).
    pub deck: DeckOptions,
}

impl WorkbenchOptions {
    pub fn new(home: PathBuf) -> Self {
        WorkbenchOptions {
            home,
            kernel_launcher: None,
            sandbox: None,
            session_python: None,
            autostart_kernel: true,
            dev_mode: cfg!(debug_assertions),
            init: crate::init::InitOptions::default(),
            kernel_source: None,
            deck: DeckOptions::default(),
        }
    }
}

/// One per app.
pub struct Workbench {
    pub opts: WorkbenchOptions,
    pub global: RwLock<GlobalConfig>,
    pub sessions: RwLock<HashMap<SessionId, Arc<SessionRuntime>>>,
    pub terminals: TerminalManager,
    pub tasks: Arc<TaskRegistry>,
    /// State of the local tooling environment, shared with every session runtime.
    pub tooling: Arc<Tooling>,
    mpl_dir: Mutex<Option<PathBuf>>,
    kernel_source: RwLock<Option<PathBuf>>,
    /// Serialises `open_session` so parallel opens of one session build one runtime (U4).
    opening: tokio::sync::Mutex<()>,
    runtimes_built: AtomicU64,
}

impl Workbench {
    /// Load the global config from `opts.home` (missing file = defaults). This is service
    /// startup: the network gate's ledger is pointed at `<home>/egress.jsonl` here, and
    /// loading the config registers the destinations it names (provider endpoints, the
    /// package index, SSH targets) with the gate.
    pub fn new(opts: WorkbenchOptions) -> ServiceResult<Workbench> {
        std::fs::create_dir_all(&opts.home)?;
        wb_net::init(&opts.home);
        let global =
            config::load_global(&opts.home).map_err(|e| ServiceError::invalid(format!("{e:#}")))?;
        let kernel_source = opts.kernel_source.clone();
        Ok(Workbench {
            opts,
            kernel_source: RwLock::new(kernel_source),
            global: RwLock::new(global),
            sessions: RwLock::new(HashMap::new()),
            terminals: TerminalManager::new(),
            tasks: Arc::new(TaskRegistry::new()),
            tooling: Tooling::ready(),
            mpl_dir: Mutex::new(None),
            opening: tokio::sync::Mutex::new(()),
            runtimes_built: AtomicU64::new(0),
        })
    }

    pub fn home(&self) -> &Path {
        &self.opts.home
    }

    pub fn global(&self) -> GlobalConfig {
        self.global
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    /// Re-read `config.toml` (models or targets edited outside the app).
    pub fn reload_global(&self) -> ServiceResult<GlobalConfig> {
        let g = config::load_global(&self.opts.home)
            .map_err(|e| ServiceError::invalid(format!("{e:#}")))?;
        if let Ok(mut w) = self.global.write() {
            *w = g.clone();
        }
        Ok(g)
    }

    /// The kernel source bundled with the app, if known.
    pub fn kernel_source(&self) -> Option<PathBuf> {
        self.kernel_source.read().ok().and_then(|k| k.clone())
    }

    /// Set the bundled kernel source (the desktop shell resolves it from its resource
    /// directory once the app is running).
    pub fn set_kernel_source(&self, dir: PathBuf) {
        if let Ok(mut k) = self.kernel_source.write() {
            *k = Some(dir);
        }
    }

    fn control_dir(&self) -> Option<PathBuf> {
        let d = self.opts.home.join("ssh");
        std::fs::create_dir_all(&d).ok()?;
        Some(d)
    }

    fn tooling_python(&self) -> String {
        runtime::tooling_python(&self.global(), &self.opts.home)
    }

    /// The directory holding `analyst_mpl`. Only a successful lookup is cached: the tooling
    /// environment may not exist yet when the first session opens.
    fn mpl_dir(&self) -> Option<PathBuf> {
        if let Some(d) = self.mpl_dir.lock().ok().and_then(|m| m.clone()) {
            return Some(d);
        }
        if self.tooling.state() != ToolingState::Ready {
            return self.kernel_source();
        }
        let found = terminal::find_mpl_dir(&self.tooling_python()).or_else(|| self.kernel_source());
        if let (Some(d), Ok(mut m)) = (&found, self.mpl_dir.lock()) {
            *m = Some(d.clone());
        }
        found
    }

    /// Environment variables never passed to child processes (provider API keys, U11).
    fn key_env_names(&self) -> Vec<String> {
        terminal::key_env_names(&self.global())
    }

    fn runtime_options(&self) -> RuntimeOptions {
        RuntimeOptions {
            tooling_python: None,
            tooling: self.tooling.clone(),
            env_remove: self.key_env_names(),
            kernel_launcher: self.opts.kernel_launcher.clone(),
            sandbox: self.opts.sandbox.clone(),
            session_python: self.opts.session_python.clone(),
            autostart_kernel: self.opts.autostart_kernel,
            control_dir: self.control_dir(),
            mpl_dir: None,
            kernel_source: self.kernel_source(),
            deck: self.opts.deck.clone(),
        }
    }

    /// Send an event to every open session (tasks that belong to no session).
    fn broadcast(&self, e: SessionEvent) {
        let sinks: Vec<Arc<SessionRuntime>> = self
            .sessions
            .read()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default();
        for rt in sinks {
            rt.emit(e.clone());
        }
    }

    /// Whether the local kernel needs the bootstrapped tooling environment
    /// (`$ANALYST_HOME/tooling`): no launcher override, no `ANALYST_TOOLING_PYTHON`, no
    /// `tooling_python` in the config, and no development checkout with a kernel venv.
    fn needs_local_tooling(&self) -> bool {
        if self.opts.kernel_launcher.is_some() {
            return false;
        }
        if std::env::var("ANALYST_TOOLING_PYTHON")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            return false;
        }
        if self
            .global()
            .tooling_python
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            return false;
        }
        // A development checkout with a kernel venv hosts the kernel itself.
        !self
            .tooling_python()
            .replace('\\', "/")
            .ends_with("python/kernel/.venv/bin/python")
    }

    /// Prepare the local tooling environment in the background when the app does not run
    /// from a development checkout (B12): `uv venv` + `uv pip install <kernel source>` under
    /// `$ANALYST_HOME/tooling`, version-checked, so later starts do nothing. Kernels wait for
    /// it; a failure (for example a missing `uv`) becomes the kernel's error state with a
    /// message that names what is missing and where it was looked for. Progress is reported
    /// as `task_log` / `task_done` to every open session. Returns the task id when work was
    /// started.
    pub fn prepare_tooling(self: &Arc<Self>) -> Option<TaskId> {
        if !self.needs_local_tooling() {
            return None;
        }
        if crate::target::bootstrap::local_is_current(&self.opts.home) {
            return None;
        }
        let source = self
            .kernel_source()
            .filter(|p| p.exists())
            .or_else(crate::target::bootstrap::find_dev_kernel_source);
        let id = self.tasks.alloc("tooling");
        let Some(source) = source else {
            let msg = "The Python tooling environment cannot be set up: the kernel source bundled with the app was not found. Reinstall the app, or set ANALYST_TOOLING_PYTHON to an interpreter that has analyst_kernel installed.".to_string();
            tracing::error!("{msg}");
            self.tooling.set(ToolingState::Failed(msg));
            return None;
        };
        self.tooling.set(ToolingState::Preparing);
        let wb = self.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let log = |line: String| {
                tracing::info!("tooling: {line}");
                wb.broadcast(SessionEvent::TaskLog {
                    task_id: task_id.clone(),
                    line,
                });
            };
            log(format!(
                "setting up the Python tooling environment in {} from {}",
                wb.opts.home.join("tooling").display(),
                source.display()
            ));
            match crate::target::bootstrap::ensure_local(&wb.opts.home, &source).await {
                Ok(outcome) => {
                    log(match outcome {
                        crate::target::bootstrap::Outcome::UpToDate(v) => {
                            format!("tooling {v} already installed")
                        }
                        crate::target::bootstrap::Outcome::Installed(v) => {
                            format!("installed tooling {v}")
                        }
                    });
                    wb.tooling.set(ToolingState::Ready);
                    wb.broadcast(SessionEvent::TaskDone {
                        task_id: task_id.clone(),
                        ok: true,
                        message: None,
                    });
                }
                Err(e) => {
                    let msg = format!("Setting up the Python tooling environment failed: {e:#}");
                    tracing::error!("{msg}");
                    log(format!("error: {msg}"));
                    wb.tooling.set(ToolingState::Failed(msg.clone()));
                    wb.broadcast(SessionEvent::TaskDone {
                        task_id: task_id.clone(),
                        ok: false,
                        message: Some(msg),
                    });
                }
            }
        });
        Some(id)
    }

    pub fn app_info(&self) -> AppInfo {
        AppInfo {
            version: crate::CORE_VERSION.to_string(),
            home_abs: self.opts.home.to_string_lossy().to_string(),
            platform: Platform::current(),
            dev_mode: self.opts.dev_mode,
        }
    }

    // ---- sessions ----

    pub fn session(&self, id: &str) -> ServiceResult<Arc<SessionRuntime>> {
        self.sessions
            .read()
            .ok()
            .and_then(|s| s.get(id).cloned())
            .ok_or_else(|| ServiceError::not_found(format!("session `{id}` is not open")))
    }

    fn open_ids(&self) -> Vec<String> {
        self.sessions
            .read()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Every stored session, newest first.
    pub fn sessions_list(&self) -> ServiceResult<Vec<SessionSummary>> {
        let open = self.open_ids();
        let mut out = Vec::new();
        for s in crate::session::list(&self.opts.home)? {
            if let Some(rt) = open
                .contains(&s.record.id.to_string())
                .then(|| self.session(s.id()).ok())
                .flatten()
            {
                out.push(rt.summary());
                continue;
            }
            let meta = runtime::load_meta(&s.dir);
            let repo = s.repo();
            out.push(SessionSummary {
                id: s.id().to_string(),
                name: meta.name.unwrap_or_else(|| {
                    repo.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| s.id().to_string())
                }),
                repo_abs: s.record.repo.clone(),
                target: s.record.target.clone(),
                model: Some(s.record.model.clone()).filter(|m| !m.is_empty() && m != "-"),
                created_at: s.record.created_at.clone(),
                last_opened_at: if meta.last_opened_at.is_empty() {
                    s.record.created_at.clone()
                } else {
                    meta.last_opened_at
                },
                open: false,
            });
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(out)
    }

    /// Create (optionally from the template) and open a session.
    pub async fn create_session(
        &self,
        repo_abs: &str,
        target: Option<&str>,
        model: Option<&str>,
        init: bool,
    ) -> ServiceResult<SessionInfo> {
        let repo = PathBuf::from(repo_abs);
        if !repo.is_absolute() {
            return Err(ServiceError::invalid("`repo_abs` must be an absolute path"));
        }
        let global = self.global();
        let target_name = target.unwrap_or(&global.defaults.target).to_string();
        config::find_target(&global, &target_name)
            .map_err(|e| ServiceError::not_found(e.to_string()))?;
        let model = match model {
            Some(m) => {
                let spec = config::ModelSpec::parse(m)
                    .map_err(|e| ServiceError::invalid(e.to_string()))?;
                spec.to_string()
            }
            None => global.defaults.model.clone().unwrap_or_default(),
        };
        if init {
            let repo2 = repo.clone();
            let opts = self.opts.init.clone();
            tokio::task::spawn_blocking(move || crate::init::init_repo(&repo2, &opts))
                .await
                .map_err(|e| ServiceError::internal(e.to_string()))?
                .map_err(|e| ServiceError::io(format!("{e:#}")))?;
        }
        if !repo.is_dir() {
            return Err(ServiceError::not_found(format!(
                "`{}` is not a directory (pass init: true to create it)",
                repo.display()
            )));
        }
        if !repo.join(".git").exists() {
            // Checkpoints and the working-tree diff need a git repo.
            crate::checkpoint::git(&repo, &["init", "-q"], None)
                .map_err(|e| ServiceError::io(format!("git init: {e:#}")))?;
        }
        let session = Session::create(&self.opts.home, &repo, &target_name, &model)
            .map_err(|e| ServiceError::io(format!("{e:#}")))?;
        let id = session.id().to_string();
        self.open_session(&id).await
    }

    /// Open a stored session (idempotent). Never waits for the kernel. Parallel calls for the
    /// same session build one runtime (U4).
    pub async fn open_session(&self, id: &str) -> ServiceResult<SessionInfo> {
        if let Ok(rt) = self.session(id) {
            return Ok(rt.info().await);
        }
        let rt = {
            let _opening = self.opening.lock().await;
            match self.session(id) {
                Ok(rt) => rt,
                Err(_) => {
                    let session = Session::open(&self.opts.home, id)
                        .map_err(|e| ServiceError::not_found(format!("{e:#}")))?;
                    let rt = SessionRuntime::open(
                        session,
                        self.global(),
                        self.runtime_options(),
                        self.tasks.clone(),
                    )?;
                    self.runtimes_built.fetch_add(1, Ordering::SeqCst);
                    self.sessions
                        .write()
                        .map_err(|_| ServiceError::internal("session table poisoned"))?
                        .insert(id.to_string(), rt.clone());
                    rt
                }
            }
        };
        Ok(rt.info().await)
    }

    /// How many session runtimes this workbench has built (tests).
    pub fn runtimes_built(&self) -> u64 {
        self.runtimes_built.load(Ordering::SeqCst)
    }

    /// Suspend a session: kernel shut down, terminals closed.
    pub async fn close_session(&self, id: &str) -> ServiceResult<()> {
        let rt = self
            .sessions
            .write()
            .ok()
            .and_then(|mut s| s.remove(id))
            .ok_or_else(|| ServiceError::not_found(format!("session `{id}` is not open")))?;
        self.terminals.close_session(id);
        rt.shutdown().await;
        Ok(())
    }

    pub async fn session_info(&self, id: &str) -> ServiceResult<SessionInfo> {
        Ok(self.session(id)?.info().await)
    }

    pub fn rename_session(&self, id: &str, name: &str) -> ServiceResult<SessionSummary> {
        if let Ok(rt) = self.session(id) {
            return rt.rename(name);
        }
        let s = Session::open(&self.opts.home, id)
            .map_err(|e| ServiceError::not_found(format!("{e:#}")))?;
        let mut meta = runtime::load_meta(&s.dir);
        let name = name.trim();
        if name.is_empty() {
            return Err(ServiceError::invalid("name must not be empty"));
        }
        meta.name = Some(name.to_string());
        runtime::save_meta(&s.dir, &meta)?;
        let list = self.sessions_list()?;
        list.into_iter()
            .find(|x| x.id == id)
            .ok_or_else(|| ServiceError::not_found(format!("session `{id}` not found")))
    }

    pub fn layout_get(&self, id: &str) -> ServiceResult<Option<String>> {
        self.session(id)?.layout_get()
    }

    pub fn layout_save(&self, id: &str, layout: &str) -> ServiceResult<()> {
        self.session(id)?.layout_save(layout)
    }

    // ---- models and targets ----

    fn is_local_provider(p: &analyst_protocol::config::ProviderConfig) -> bool {
        let url = p.effective_base_url();
        url.contains("localhost")
            || url.contains("127.0.0.1")
            || url.contains("0.0.0.0")
            || url.contains("[::1]")
    }

    fn model_entry(&self, global: &GlobalConfig, provider: &str, id: &str) -> Option<ModelEntry> {
        let p = global.providers.iter().find(|p| p.name == provider)?;
        let key = format!("{provider}/{id}");
        let caps = capabilities::cached(&self.opts.home, &key);
        let overrides = global
            .models
            .iter()
            .find(|m| m.provider == provider && m.id == id);
        let capabilities = match (caps, overrides) {
            (Some(c), _) => Some(ModelCapabilitiesView {
                context_window: c.context_window,
                tools: crate::model_client::tool_support(c.tools),
                vision: c.vision,
            }),
            (None, Some(o)) if o.context_window.is_some() && o.tools.is_some() => {
                Some(ModelCapabilitiesView {
                    context_window: o
                        .context_window
                        .unwrap_or(capabilities::DEFAULT_CONTEXT_WINDOW),
                    tools: o
                        .tools
                        .unwrap_or(analyst_protocol::config::ToolSupport::Native),
                    vision: o.vision.unwrap_or(false),
                })
            }
            _ => None,
        };
        Some(ModelEntry {
            name: key,
            provider: provider.to_string(),
            id: id.to_string(),
            kind: p.kind,
            local: p.kind == ProviderKind::OpenAiCompatible && Self::is_local_provider(p),
            capabilities,
        })
    }

    /// Models from the global config plus cached capabilities.
    pub fn models_list(&self) -> Vec<ModelEntry> {
        let global = self.global();
        let mut out: Vec<ModelEntry> = Vec::new();
        if let Some(d) = &global.defaults.model {
            if let Ok(spec) = config::ModelSpec::parse(d) {
                if let Some(e) = self.model_entry(&global, &spec.provider, &spec.id) {
                    out.push(e);
                }
            }
        }
        for m in &global.models {
            if out.iter().any(|e| e.provider == m.provider && e.id == m.id) {
                continue;
            }
            if let Some(e) = self.model_entry(&global, &m.provider, &m.id) {
                out.push(e);
            }
        }
        if let Ok(cache) = capabilities::load_cache(&self.opts.home) {
            for key in cache.keys() {
                if let Ok(spec) = config::ModelSpec::parse(key) {
                    if out.iter().any(|e| e.name == *key) {
                        continue;
                    }
                    if let Some(e) = self.model_entry(&global, &spec.provider, &spec.id) {
                        out.push(e);
                    }
                }
            }
        }
        out
    }

    /// Run the capability probe for a model and cache the result.
    pub async fn model_probe(&self, model: &str) -> ServiceResult<ModelEntry> {
        let global = self.global();
        let spec =
            config::ModelSpec::parse(model).map_err(|e| ServiceError::invalid(e.to_string()))?;
        let resolved = config::resolve_model(&global, &spec)
            .map_err(|e| ServiceError::not_found(e.to_string()))?;
        let client = crate::model_client::client_for_in(&self.opts.home, &resolved)
            .map_err(|e| ServiceError::invalid(format!("{e:#}")))?;
        let info_window =
            crate::model_client::model_info_window(&self.opts.home, None, &resolved).await;
        crate::model_client::ensure_capabilities(
            &self.opts.home,
            client.as_ref(),
            &resolved,
            info_window,
            true,
        )
        .await
        .map_err(|e| ServiceError::internal(format!("probe failed: {e:#}")))?;
        self.model_entry(&global, &spec.provider, &spec.id)
            .ok_or_else(|| ServiceError::not_found(format!("model `{model}` not found")))
    }

    /// One row per configured provider: whether a key is available and where it comes from
    /// (`"env"`: the process environment; `"stored"`: `$ANALYST_HOME/.env`; or null). A
    /// key that only a repo's own `.env` provides is not listed here: it belongs to that
    /// session. Key values never leave the backend.
    pub fn provider_keys_status(&self) -> Vec<ProviderKeyStatus> {
        let global = self.reload_global().unwrap_or_else(|_| self.global());
        global
            .providers
            .iter()
            .map(|p| {
                let source =
                    config::api_key_lookup(&self.opts.home, None, p).map(|(_, s)| match s {
                        config::KeySource::Env => "env".to_string(),
                        config::KeySource::Stored => "stored".to_string(),
                    });
                ProviderKeyStatus {
                    provider: p.name.clone(),
                    has_key: source.is_some(),
                    source,
                    api_key_env: Some(config::key_env_name(p)),
                }
            })
            .collect()
    }

    /// Store a provider's key in `$ANALYST_HOME/.env` (mode 0600) under the provider's
    /// `api_key_env` name, or the derived `<PROVIDER NAME>_API_KEY` when none is configured;
    /// an empty key removes it. Conversations pick the change up on their next turn.
    pub fn provider_key_set(&self, provider: &str, key: &str) -> ServiceResult<()> {
        let global = self.reload_global().unwrap_or_else(|_| self.global());
        let Some(provider) = global.providers.iter().find(|p| p.name == provider) else {
            return Err(ServiceError::not_found(format!(
                "provider `{provider}` is not configured"
            )));
        };
        if key.contains(['\n', '\r']) {
            return Err(ServiceError::invalid("the key must be a single line"));
        }
        config::set_api_key(&self.opts.home, provider, key)
            .map_err(|e| ServiceError::io(format!("{e:#}")))?;
        let open: Vec<Arc<SessionRuntime>> = self
            .sessions
            .read()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default();
        for rt in open {
            rt.invalidate_models();
        }
        Ok(())
    }

    pub fn targets_list(&self) -> Vec<TargetInfo> {
        let global = self.global();
        let mut out = vec![TargetInfo {
            name: "local".into(),
            kind: TargetKind::Local,
            host: None,
            remote_root: None,
            sandbox: self.opts.sandbox.clone().unwrap_or_else(|| "auto".into()),
            data_dirs: vec![],
            bootstrapped: Some(true),
        }];
        for t in &global.targets {
            if t.name == "local" {
                continue;
            }
            out.push(TargetInfo {
                name: t.name.clone(),
                kind: t.kind,
                host: t.host.clone(),
                remote_root: Some(t.remote_root.clone().unwrap_or_else(|| "~/analyst".into())),
                sandbox: t.sandbox.clone().unwrap_or_else(|| "auto".into()),
                data_dirs: t.data_dirs.clone(),
                bootstrapped: None,
            });
        }
        out
    }

    /// Bootstrap a remote target as a task. Task events go to every open session's channel
    /// (the command has no session argument).
    pub fn target_bootstrap(&self, name: &str) -> ServiceResult<TaskId> {
        let global = self.global();
        let cfg = config::find_target(&global, name)
            .map_err(|e| ServiceError::not_found(e.to_string()))?;
        let target = Target::from_config(&cfg).map_err(|e| ServiceError::invalid(e.to_string()))?;
        if !target.is_remote() {
            return Err(ServiceError::invalid("the local target needs no bootstrap"));
        }
        let id = self.tasks.alloc("bootstrap");
        let (tx, _rx) = tokio::sync::broadcast::channel(256);
        let logger = TaskLogger::new(id.clone(), tx.clone());
        // Fan out to every open session.
        let sinks: Vec<tokio::sync::broadcast::Sender<SessionEvent>> = self
            .sessions
            .read()
            .map(|s| s.values().map(|r| r.events()).collect())
            .unwrap_or_default();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            while let Ok(e) = rx.recv().await {
                for s in &sinks {
                    let _ = s.send(e.clone());
                }
                if matches!(e, SessionEvent::TaskDone { .. }) {
                    break;
                }
            }
        });
        let control_dir = self.control_dir();
        tokio::spawn(async move {
            let r: anyhow::Result<()> = async {
                let wheel = crate::target::bootstrap::find_dev_wheel();
                logger.log(format!("bootstrapping {}", target.name()));
                match crate::target::bootstrap::ensure(
                    &target,
                    control_dir.as_deref(),
                    wheel.as_deref(),
                )
                .await?
                {
                    crate::target::bootstrap::Outcome::UpToDate(v) => {
                        logger.log(format!("tooling {v} already installed"))
                    }
                    crate::target::bootstrap::Outcome::Installed(v) => {
                        logger.log(format!("installed tooling {v}"))
                    }
                }
                Ok(())
            }
            .await;
            logger.finish(r);
        });
        Ok(id)
    }

    // ---- terminals ----

    /// Open a terminal in the session's repo (or its mirror on a remote target). The shell
    /// has no network access where the platform can enforce that: the result says whether
    /// it is isolated and how, and the renderer shows it.
    pub async fn terminal_open(
        &self,
        session: &str,
        cols: u16,
        rows: u16,
    ) -> ServiceResult<TerminalOpened> {
        let rt = self.session(session)?;
        let id = format!("term-{}", crate::util::stamped_id());
        let target = rt.target();
        // A remote shell is wrapped with bwrap on the host when the host's probe says it
        // works there. A failed probe is not fatal: the shell opens, reported as unisolated.
        let remote_no_network = match &target {
            Target::Ssh { host, .. } => {
                crate::egress::remote_bwrap_works(host, self.control_dir().as_deref())
                    .await
                    .unwrap_or(false)
            }
            Target::Local => false,
        };
        let spec = TerminalSpec {
            repo: rt.repo.clone(),
            target,
            remote_no_network,
            control_dir: self.control_dir(),
            capture: CaptureEnv::new(&rt.repo, &rt.out_dir(), self.mpl_dir()),
            shell: None,
            env_remove: self.key_env_names(),
            cols,
            rows,
        };
        let t = Terminal::open(id.clone(), session, &spec, rt.hub())?;
        let opened = TerminalOpened {
            terminal_id: id,
            network_isolated: t.isolation.is_isolated(),
            network_isolation: t.isolation.label().to_string(),
            network_isolation_note: match &t.isolation {
                wb_sandbox::no_network::NetIsolation::None(why) => Some(why.clone()),
                _ => None,
            },
        };
        self.terminals.insert(t);
        Ok(opened)
    }

    pub fn terminal_write(&self, terminal_id: &str, data: &str) -> ServiceResult<()> {
        self.terminals.get(terminal_id)?.write(data)
    }

    pub fn terminal_resize(&self, terminal_id: &str, cols: u16, rows: u16) -> ServiceResult<()> {
        self.terminals.get(terminal_id)?.resize(cols, rows)
    }

    pub fn terminal_close(&self, terminal_id: &str) -> ServiceResult<()> {
        let t = self.terminals.remove(terminal_id).ok_or_else(|| {
            ServiceError::not_found(format!("terminal `{terminal_id}` is not open"))
        })?;
        t.close();
        Ok(())
    }

    /// Close every session (app exit).
    pub async fn shutdown(&self) {
        let ids = self.open_ids();
        for id in ids {
            let _ = self.close_session(&id).await;
        }
    }
}

#[cfg(test)]
mod tests;
