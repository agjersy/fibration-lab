//! The desktop service layer (IPC.md): a `Workbench` holding one `ProjectRuntime` per open
//! project. The Tauri crate is thin glue over the methods here, so everything is testable
//! without a window. Shaped after Personal Analyst `crates/core/src/service/`.

mod extras;
pub use extras::image_size;

use crate::agent::dispatch::{self, Change, ToolState};
use crate::agent::tools::Mode as AgentMode;
use crate::agent::{ApprovalDecision, ApprovalRequest, Approver};
use crate::deck_store::{DeckError, Position};
use crate::kernel::KernelError;
use crate::project::Project;
use crate::session::{self, ResolvedModel};
use crate::util::{now_iso, sha256_hex, write_atomic};
use crate::workspace::{OpenOptions, Workspace};
use async_trait::async_trait;
use present_protocol::config::GlobalConfig;
use present_protocol::deck::{DeckMode, Slide};
use present_protocol::ids::SlideId;
use present_protocol::ui::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, oneshot};

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

/// A project the workbench knows about.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Known {
    id: String,
    path: String,
    name: String,
    created_at: String,
    last_opened_at: String,
    #[serde(default)]
    model: Option<String>,
}

/// Supplies the model for a conversation. Tests install a scripted one.
pub type ModelFactory = Arc<
    dyn Fn(Option<&str>) -> futures::future::BoxFuture<'static, anyhow::Result<ResolvedModel>>
        + Send
        + Sync,
>;

pub struct Workbench {
    pub home: PathBuf,
    open: Mutex<HashMap<String, Arc<ProjectRuntime>>>,
    model_factory: Mutex<Option<ModelFactory>>,
    pub options: Mutex<OpenOptions>,
}

fn project_id(path: &Path) -> String {
    format!("p-{}", &sha256_hex(path.to_string_lossy().as_bytes())[..12])
}

impl Workbench {
    pub fn new(home: &Path) -> Workbench {
        let _ = std::fs::create_dir_all(home);
        // Every network decision from here on is recorded in `<home>/egress.jsonl`.
        wb_net::init(home);
        Workbench {
            home: home.to_path_buf(),
            open: Mutex::new(HashMap::new()),
            model_factory: Mutex::new(None),
            options: Mutex::new(OpenOptions::new(home)),
        }
    }

    pub fn set_model_factory(&self, factory: ModelFactory) {
        *self.model_factory.lock().unwrap_or_else(|e| e.into_inner()) = Some(factory);
    }

    fn global(&self) -> GlobalConfig {
        crate::config::load_global(&self.home).unwrap_or_default()
    }

    fn registry_path(&self) -> PathBuf {
        self.home.join("projects.json")
    }

    fn known(&self) -> Vec<Known> {
        std::fs::read(self.registry_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn remember(&self, entry: Known) -> R<()> {
        let mut all = self.known();
        all.retain(|k| k.id != entry.id);
        all.push(entry);
        let text = serde_json::to_vec_pretty(&all).map_err(|e| err(IpcErrorCode::Internal, e))?;
        write_atomic(&self.registry_path(), &text).map_err(|e| err(IpcErrorCode::Io, e))
    }

    pub fn app_info(&self) -> AppInfo {
        let first = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .next()
            .cloned();
        // What an open kernel runs under; before a project is open, what one would get.
        let kernel_network = first
            .as_ref()
            .map(|rt| rt.ws.kernel.network_isolation().clone())
            .unwrap_or_else(|| wb_sandbox::no_network::probe().clone());
        let hello = first
            .map(|rt| rt.ws.kernel.hello().clone())
            .unwrap_or(Value::Null);
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            home_abs: self.home.to_string_lossy().into_owned(),
            platform: if cfg!(windows) {
                Platform::Windows
            } else if cfg!(target_os = "macos") {
                Platform::Macos
            } else {
                Platform::Linux
            },
            dev_mode: cfg!(debug_assertions),
            engines: serde_json::from_value(hello["engines"].clone()).unwrap_or_default(),
            sandbox: serde_json::from_value(hello["sandbox"].clone()).ok(),
            embedding: serde_json::from_value(hello["embedding"].clone()).ok(),
            kernel_network: kernel_network.label().to_string(),
            kernel_network_detail: match &kernel_network {
                wb_sandbox::no_network::NetIsolation::None(why) => Some(why.clone()),
                _ => None,
            },
        }
    }

    pub fn projects_list(&self) -> Vec<ProjectSummary> {
        let open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<ProjectSummary> = self
            .known()
            .into_iter()
            .filter(|k| Path::new(&k.path).join("deck").join("deck.json").is_file())
            .map(|k| {
                let deck = crate::deck_store::DeckStore::new(Path::new(&k.path))
                    .read_deck()
                    .ok();
                ProjectSummary {
                    open: open.contains_key(&k.id),
                    template_name: deck
                        .as_ref()
                        .map(|d| d.template.rsplit('/').next().unwrap_or("").to_string())
                        .unwrap_or_default(),
                    deck_mode: deck.as_ref().map(|d| d.mode).unwrap_or_default(),
                    slides: deck
                        .as_ref()
                        .map(|d| d.slide_ids().count() as u32)
                        .unwrap_or(0),
                    id: k.id,
                    name: k.name,
                    path_abs: k.path,
                    model: k.model,
                    created_at: k.created_at,
                    last_opened_at: k.last_opened_at,
                }
            })
            .collect();
        out.sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at));
        out
    }

    pub async fn project_create(
        &self,
        path_abs: &str,
        name: &str,
        template_abs: &str,
        deck_mode: Option<DeckMode>,
    ) -> R<ProjectInfo> {
        let root = PathBuf::from(path_abs);
        Project::create(
            &root,
            name,
            Path::new(template_abs),
            deck_mode.unwrap_or_default(),
        )
        .map_err(|e| IpcError::invalid(format!("{e:#}")))?;
        if deck_mode == Some(DeckMode::Code) {
            let script = "from present_sdk import Deck\n\n\ndef main():\n    deck = Deck.open_template()\n    deck.save()\n\n\nif __name__ == \"__main__\":\n    main()\n";
            write_atomic(&root.join("deck").join("build.py"), script.as_bytes())
                .map_err(|e| err(IpcErrorCode::Io, e))?;
        }
        self.project_open(path_abs).await
    }

    pub async fn project_open(&self, path_abs: &str) -> R<ProjectInfo> {
        let root = PathBuf::from(path_abs)
            .canonicalize()
            .map_err(|e| IpcError::not_found(format!("{path_abs}: {e}")))?;
        let id = project_id(&root);
        let existing = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned();
        if let Some(rt) = existing {
            return rt.info().await;
        }
        let options = self
            .options
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let ws = Workspace::open(&root, &options).await.map_err(|e| {
            let text = format!("{e:#}");
            if text.contains("not a Present project") {
                IpcError::not_found(text)
            } else {
                err(IpcErrorCode::KernelOffline, text)
            }
        })?;
        let deck = ws.deck().read_deck()?;
        let previous = self.known().into_iter().find(|k| k.id == id);
        let now = now_iso();
        let model = previous
            .as_ref()
            .and_then(|k| k.model.clone())
            .or_else(|| self.global().defaults.model.clone());
        self.remember(Known {
            id: id.clone(),
            path: root.to_string_lossy().into_owned(),
            name: deck.title.clone(),
            created_at: previous
                .map(|k| k.created_at)
                .unwrap_or_else(|| now.clone()),
            last_opened_at: now,
            model: model.clone(),
        })?;
        let (events, _) = broadcast::channel(2048);
        let rt = Arc::new(ProjectRuntime {
            id: id.clone(),
            ws: Arc::new(ws),
            home: self.home.clone(),
            events,
            state: Mutex::new(RuntimeState {
                model,
                ..Default::default()
            }),
            model_factory: self
                .model_factory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            tasks: std::sync::atomic::AtomicU64::new(1),
        });
        rt.load_state();
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, rt.clone());
        rt.emit(ProjectEvent::KernelState {
            state: KernelState::Ready,
            message: None,
        });
        rt.info().await
    }

    pub async fn project_close(&self, id: &str) -> R<()> {
        let rt = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(rt) = rt {
            rt.save_state();
            rt.ws.close().await;
        }
        Ok(())
    }

    pub fn project(&self, id: &str) -> R<Arc<ProjectRuntime>> {
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| IpcError::not_found(format!("project {id} is not open")))
    }

    pub fn models_list(&self) -> Vec<ModelEntry> {
        let global = self.global();
        let cache = wb_providers::capabilities::load_cache(&self.home).unwrap_or_default();
        let mut names: Vec<(String, String)> = global
            .models
            .iter()
            .map(|m| (m.provider.clone(), m.id.clone()))
            .collect();
        if let Some((p, m)) = global
            .defaults
            .model
            .as_deref()
            .and_then(|d| d.split_once('/'))
        {
            if !names.iter().any(|(a, b)| a == p && b == m) {
                names.insert(0, (p.to_string(), m.to_string()));
            }
        }
        names
            .into_iter()
            .map(|(provider, id)| {
                let name = format!("{provider}/{id}");
                let caps = cache.get(&name);
                let location = global
                    .providers
                    .iter()
                    .find(|p| p.name == provider)
                    .map(|p| {
                        serde_json::to_value(p.effective_location())
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default()
                    })
                    .unwrap_or_else(|| "external".into());
                ModelEntry {
                    name,
                    provider,
                    location,
                    capabilities: caps.map(capabilities_view),
                    degraded: caps.is_some_and(|c| c.degraded),
                    degraded_reasons: caps.map(|c| c.degraded_reasons.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }

    pub async fn model_probe(&self, model: &str) -> R<ModelEntry> {
        let resolved = session::resolve_model(&self.home, &self.global(), Some(model), true)
            .await
            .map_err(|e| IpcError::invalid(format!("{e:#}")))?;
        Ok(ModelEntry {
            name: resolved.name.clone(),
            provider: resolved.provider.name.clone(),
            location: serde_json::to_value(resolved.provider.location)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            capabilities: Some(capabilities_view(&resolved.caps)),
            degraded: resolved.caps.degraded,
            degraded_reasons: resolved.caps.degraded_reasons.clone(),
        })
    }

    pub fn audit_export(&self, from: &str, to: &str, path_abs: &str) -> R<usize> {
        crate::audit::AuditLog::new(&self.home)
            .export_csv(from, to, Path::new(path_abs))
            .map_err(|e| err(IpcErrorCode::Io, e))
    }
}

fn capabilities_view(c: &wb_providers::model::ModelCapabilities) -> CapabilitiesView {
    CapabilitiesView {
        context_window: c.context_window,
        tools: serde_json::to_value(c.tools)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        vision: c.vision,
        documents: c.documents,
        caching: c.caching,
        probed_at: if c.probed_at.is_empty() {
            None
        } else {
            Some(c.probed_at.clone())
        },
    }
}

#[derive(Default)]
struct ConvState {
    mode: Option<AgentMode>,
    model: Option<String>,
    busy: bool,
    cancel: Option<tokio::task::AbortHandle>,
}

#[derive(Default)]
struct RuntimeState {
    model: Option<String>,
    active: String,
    conversations: BTreeMap<String, ConvState>,
    pending: HashMap<String, oneshot::Sender<ProposalDecision>>,
    diagnostics: Vec<Value>,
    thumbs: BTreeMap<String, (String, String)>,
    /// Renderer-made preview images: slide id to (path, slide hash when made).
    previews: BTreeMap<String, (String, String)>,
    policy_patch: PolicyPatch,
    disabled_skills: BTreeSet<String>,
    layout: Option<Value>,
    exports: Vec<ExportView>,
    tool_state_template: Option<Value>,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedState {
    active: String,
    #[serde(default)]
    layout: Option<Value>,
    #[serde(default)]
    exports: Vec<ExportView>,
    #[serde(default)]
    modes: BTreeMap<String, String>,
    #[serde(default)]
    disabled_skills: BTreeSet<String>,
}

pub struct ProjectRuntime {
    pub id: String,
    pub ws: Arc<Workspace>,
    home: PathBuf,
    events: broadcast::Sender<ProjectEvent>,
    state: Mutex<RuntimeState>,
    model_factory: Option<ModelFactory>,
    tasks: std::sync::atomic::AtomicU64,
}

/// Bridges the agent's approvals to the renderer: emit a `proposal`, wait for `proposal_respond`.
struct BridgeApprover {
    rt: Arc<ProjectRuntime>,
    conversation: String,
    turn: String,
}

#[async_trait]
impl Approver for BridgeApprover {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let (tx, rx) = oneshot::channel();
        self.rt.lock().pending.insert(request.id.clone(), tx);
        let view = self
            .rt
            .proposal_view(request, &self.conversation, &self.turn);
        self.rt.emit(ProjectEvent::Proposal {
            proposal: Box::new(view),
        });
        let decision = rx.await.unwrap_or(ProposalDecision::Reject {
            note: Some("the turn was cancelled".into()),
        });
        let (label, out) = match decision {
            ProposalDecision::Accept => ("accept", ApprovalDecision::Accept),
            ProposalDecision::Reject { note } => ("reject", ApprovalDecision::Reject { note }),
            ProposalDecision::AcceptModified {
                slides,
                outline,
                code,
            } => match modified_change(request.change.as_ref(), slides, outline, code) {
                Some(change) => (
                    "accept_modified",
                    ApprovalDecision::AcceptModified(Box::new(change)),
                ),
                None => ("accept", ApprovalDecision::Accept),
            },
        };
        self.rt.emit(ProjectEvent::ProposalResolved {
            proposal_id: request.id.clone(),
            decision: label.into(),
            by: "user".into(),
        });
        out
    }
}

fn modified_change(
    original: Option<&Change>,
    slides: Option<Vec<Slide>>,
    outline: Option<Vec<OutlineRow>>,
    code: Option<String>,
) -> Option<Change> {
    match original? {
        Change::SlideCreate { after, section, .. } => Some(Change::SlideCreate {
            slide: Box::new(slides?.into_iter().next()?),
            after: after.clone(),
            section: section.clone(),
        }),
        Change::SlideUpdate {
            before,
            expected_hash,
            ..
        } => Some(Change::SlideUpdate {
            before: before.clone(),
            after: Box::new(slides?.into_iter().next()?),
            expected_hash: expected_hash.clone(),
        }),
        Change::Outline { .. } => Some(Change::Outline {
            rows: outline?
                .into_iter()
                .map(|r| dispatch::OutlineRow {
                    slide_id: r.slide_id,
                    title: r.title,
                    message: r.message,
                    layout: r.layout,
                    sources: r.sources,
                })
                .collect(),
        }),
        Change::CodeEdit { path, original, .. } => Some(Change::CodeEdit {
            path: path.clone(),
            original: original.clone(),
            proposed: code?,
        }),
        _ => None,
    }
}

impl ProjectRuntime {
    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProjectEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event: ProjectEvent) {
        let _ = self.events.send(event);
    }

    fn state_path(&self) -> PathBuf {
        session::transcript_store(&self.home, &self.ws.project.root)
            .root()
            .join("state.json")
    }

    fn load_state(&self) {
        let saved: SavedState = std::fs::read(self.state_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut st = self.lock();
        st.active = if saved.active.is_empty() {
            "main".into()
        } else {
            saved.active
        };
        st.layout = saved.layout;
        st.exports = saved.exports;
        st.disabled_skills = saved.disabled_skills;
        for (conv, mode) in saved.modes {
            st.conversations.entry(conv).or_default().mode = mode.parse().ok();
        }
        let active = st.active.clone();
        st.conversations.entry(active).or_default();
    }

    fn save_state(&self) {
        let st = self.lock();
        let saved = SavedState {
            active: st.active.clone(),
            layout: st.layout.clone(),
            exports: st.exports.clone(),
            modes: st
                .conversations
                .iter()
                .filter_map(|(k, c)| c.mode.map(|m| (k.clone(), m.as_str().to_string())))
                .collect(),
            disabled_skills: st.disabled_skills.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&saved) {
            let _ = write_atomic(&self.state_path(), &bytes);
        }
    }

    fn next_task(&self) -> String {
        format!(
            "task-{}",
            self.tasks.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    }

    fn policy(&self) -> R<wb_policy::Policy> {
        let config = self.ws.project.config().map_err(internal)?;
        let base = session::policy_for(&config).map_err(internal)?;
        let patch = self.lock().policy_patch.clone();
        let mut p = base;
        let level = |s: &str| match s {
            "auto" => wb_policy::Level::Auto,
            "deny" => wb_policy::Level::Deny,
            _ => wb_policy::Level::Ask,
        };
        if let Some(e) = &patch.edits {
            p = p.with_edits(match e.as_str() {
                "auto" => wb_policy::EditGate::Auto,
                "auto-new-slides" | "auto-new" => wb_policy::EditGate::AutoNew,
                "deny" => wb_policy::EditGate::Deny,
                _ => wb_policy::EditGate::Review,
            });
        }
        for (class, value) in [
            ("code", &patch.code),
            ("skills_scripts", &patch.skills_scripts),
            ("export", &patch.export),
        ] {
            if let Some(v) = value {
                p = p.with_class(class, level(v));
            }
        }
        if let Some(n) = patch.max_steps {
            p = p.with_max_steps(n);
        }
        Ok(p)
    }

    fn policy_view(&self) -> PolicyView {
        let Ok(p) = self.policy() else {
            return PolicyView {
                edits: "review".into(),
                code: "ask".into(),
                skills_scripts: "ask".into(),
                code_mode: "allow".into(),
                export: "auto".into(),
                max_steps: 60,
            };
        };
        PolicyView {
            edits: match p.edits {
                wb_policy::EditGate::Review => "review",
                wb_policy::EditGate::AutoNew => "auto-new-slides",
                wb_policy::EditGate::Auto => "auto",
                wb_policy::EditGate::Deny => "deny",
            }
            .into(),
            code: p.level("code").as_str().into(),
            skills_scripts: p.level("skills_scripts").as_str().into(),
            code_mode: if p.denied_tools.contains("deck.switch_mode") {
                "deny"
            } else {
                "allow"
            }
            .into(),
            export: p.level("export").as_str().into(),
            max_steps: p.max_steps,
        }
    }

    pub fn set_policy(&self, patch: PolicyPatch) -> PolicyView {
        {
            let mut st = self.lock();
            let cur = &mut st.policy_patch;
            cur.edits = patch.edits.or(cur.edits.take());
            cur.code = patch.code.or(cur.code.take());
            cur.skills_scripts = patch.skills_scripts.or(cur.skills_scripts.take());
            cur.code_mode = patch.code_mode.or(cur.code_mode.take());
            cur.export = patch.export.or(cur.export.take());
            cur.max_steps = patch.max_steps.or(cur.max_steps);
        }
        self.policy_view()
    }

    fn conversations(&self) -> Vec<ConversationSummary> {
        let store = session::transcript_store(&self.home, &self.ws.project.root);
        let listed: BTreeMap<String, wb_transcript::ConversationSummary> = store
            .list()
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        let st = self.lock();
        let mut ids: BTreeSet<String> = listed.keys().cloned().collect();
        ids.extend(st.conversations.keys().cloned());
        ids.into_iter()
            .map(|id| {
                let conv = st.conversations.get(&id);
                let summary = listed.get(&id);
                ConversationSummary {
                    title: summary
                        .map(|s| s.title.clone())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| "New conversation".into()),
                    model: conv
                        .and_then(|c| c.model.clone())
                        .or_else(|| st.model.clone()),
                    mode: to_ui_mode(conv.and_then(|c| c.mode).unwrap_or(AgentMode::Build)),
                    created_at: summary.map(|s| s.created.clone()).unwrap_or_default(),
                    turns: summary.map(|s| s.turns as u32).unwrap_or(0),
                    busy: conv.is_some_and(|c| c.busy),
                    id,
                }
            })
            .collect()
    }

    async fn template(&self) -> R<present_protocol::template::TemplateDescription> {
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

    pub async fn info(&self) -> R<ProjectInfo> {
        let deck = self.ws.deck().read_deck()?;
        let (active, model) = {
            let st = self.lock();
            (st.active.clone(), st.model.clone())
        };
        Ok(ProjectInfo {
            summary: ProjectSummary {
                id: self.id.clone(),
                name: deck.title.clone(),
                path_abs: self.ws.project.root.to_string_lossy().into_owned(),
                template_name: deck.template.rsplit('/').next().unwrap_or("").to_string(),
                deck_mode: deck.mode,
                slides: deck.slide_ids().count() as u32,
                model,
                created_at: self
                    .ws
                    .project
                    .config()
                    .map(|c| c.project.created)
                    .unwrap_or_default(),
                last_opened_at: now_iso(),
                open: true,
            },
            policy: self.policy_view(),
            kernel: KernelState::Ready,
            conversations: self.conversations(),
            active_conversation: active,
            template: self.template().await?,
            head_snapshot: self
                .ws
                .snapshots()
                .head()
                .ok()
                .flatten()
                .map(|s| s.to_string()),
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
            self.emit(ProjectEvent::DeckChanged {
                deck: Box::new(deck),
                changed: changed.clone(),
                removed,
                head_snapshot: self
                    .ws
                    .snapshots()
                    .head()
                    .ok()
                    .flatten()
                    .map(|s| s.to_string()),
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
        let typed: Vec<present_protocol::kernel::Diagnostic> = diags
            .iter()
            .filter_map(|d| serde_json::from_value(d.clone()).ok())
            .collect();
        self.emit(ProjectEvent::Diagnostics {
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
                self.emit(ProjectEvent::SlideView {
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
        F: FnOnce(Arc<ProjectRuntime>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = R<Option<String>>> + Send + 'static,
    {
        let task_id = self.next_task();
        let rt = self.clone();
        let id = task_id.clone();
        self.emit(ProjectEvent::TaskProgress {
            task_id: id.clone(),
            fraction: None,
            message: label.to_string(),
        });
        tokio::spawn(async move {
            let outcome = f(rt.clone()).await;
            match outcome {
                Ok(message) => rt.emit(ProjectEvent::TaskDone {
                    task_id: id,
                    ok: true,
                    message,
                }),
                Err(e) => rt.emit(ProjectEvent::TaskDone {
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
                    rt.emit(ProjectEvent::RefsChanged { refs });
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
        self.emit(ProjectEvent::RefsChanged {
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
        self.emit(ProjectEvent::RefsChanged {
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
            let parsed: present_protocol::deck::Table =
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

    // ---- history and exports ------------------------------------------------------------------

    pub fn history_list(&self, limit: Option<usize>) -> R<Vec<SnapshotView>> {
        let snaps = self.ws.snapshots();
        let mut out: Vec<SnapshotView> = snaps
            .log()
            .map_err(internal)?
            .into_iter()
            .rev()
            .take(limit.unwrap_or(100))
            .map(|r| SnapshotView {
                slides: snaps
                    .manifest(&r.snapshot_id)
                    .map(|m| {
                        m.files
                            .iter()
                            .filter(|f| f.path.starts_with("deck/slides/"))
                            .count() as u32
                    })
                    .unwrap_or(0),
                id: r.snapshot_id.to_string(),
                created: r.ts,
                reason: serde_json::to_value(r.reason)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                label: r.label,
                turn: r.turn_id,
                thumb: None,
            })
            .collect();
        out.truncate(limit.unwrap_or(100));
        Ok(out)
    }

    fn after_restore(self: &Arc<Self>, id: present_protocol::ids::SnapshotId) -> R<SnapshotView> {
        self.lock().tool_state_template = None;
        let ids: Vec<String> = self
            .ws
            .deck()
            .read_deck()?
            .slide_ids()
            .map(|s| s.to_string())
            .collect();
        self.deck_changed(ids, vec![], "restore");
        self.history_list(Some(1))?
            .into_iter()
            .find(|s| s.id == id.as_str())
            .ok_or_else(|| IpcError::internal("restored snapshot not found"))
    }

    pub async fn history_restore(self: &Arc<Self>, snapshot_id: &str) -> R<SnapshotView> {
        // Not while a check or render is reading the deck files.
        let _scratch = self.ws.scratch_lock.lock().await;
        let id = self
            .ws
            .snapshots()
            .restore(
                &snapshot_id
                    .parse()
                    .map_err(|_| IpcError::invalid("not a snapshot id"))?,
            )
            .map_err(|e| IpcError::not_found(format!("{e:#}")))?;
        self.after_restore(id)
    }

    pub async fn history_revert_turn(self: &Arc<Self>, turn: &str) -> R<SnapshotView> {
        let _scratch = self.ws.scratch_lock.lock().await;
        let id = self
            .ws
            .snapshots()
            .revert_turn(turn)
            .map_err(|e| IpcError::not_found(format!("{e:#}")))?;
        self.after_restore(id)
    }

    pub fn exports_list(&self) -> Vec<ExportView> {
        self.lock().exports.clone()
    }

    pub fn export_run(self: &Arc<Self>, format: String) -> String {
        self.spawn_task(&format!("Exporting {format}"), move |rt| async move {
            let exported = rt.ws.export(&format, None).await.map_err(|e| {
                let text = format!("{e:#}");
                if text.contains("engine") {
                    err(IpcErrorCode::EngineUnavailable, text)
                } else {
                    IpcError::internal(text)
                }
            })?;
            let view = ExportView {
                id: format!(
                    "x-{}",
                    exported.snapshot_id.as_str().trim_start_matches("h-")
                ),
                format: exported.format,
                preset: None,
                path: exported.path.clone(),
                snapshot_id: exported.snapshot_id.to_string(),
                created_at: now_iso(),
                errors: exported.errors as u32,
                warnings: exported.warnings as u32,
                engine: exported.engine,
            };
            rt.lock().exports.insert(0, view.clone());
            rt.save_state();
            rt.emit(ProjectEvent::ExportDone { export: view });
            Ok(Some(exported.path))
        })
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
                            rt.emit(ProjectEvent::BuildOutput {
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
                            rt.emit(ProjectEvent::BuildOutput {
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

    pub fn deck_switch_mode(self: &Arc<Self>, to: DeckMode, via: Option<String>) -> String {
        self.spawn_task("Switching deck mode", move |rt| async move {
            let current = rt.ws.deck().read_deck()?.mode;
            match (current, to) {
                (DeckMode::Structured, DeckMode::Code) => {
                    if rt.policy_view().code_mode == "deny" {
                        return Err(IpcError::new(IpcErrorCode::PolicyDenied, "code generation mode is switched off for this project"));
                    }
                    let mut scratch = ToolState::default();
                    dispatch::apply(&rt.ws, &mut scratch, &Change::ModeSwitch { reason: "requested by the user".into() }).await.map_err(IpcError::invalid)?;
                }
                (DeckMode::Code, DeckMode::Structured) => {
                    if via.as_deref() != Some("revert") {
                        return Err(IpcError::invalid("switching back needs via: \"revert\" (importing a built deck is not available yet)"));
                    }
                    let log = rt.ws.snapshots().log().map_err(internal)?;
                    let before = log.iter().rev().find(|r| r.reason == present_protocol::config::SnapshotReason::ModeSwitch).ok_or_else(|| IpcError::not_found("no snapshot from before the switch to code mode"))?;
                    rt.ws.snapshots().restore(&before.snapshot_id).map_err(internal)?;
                }
                _ => return Ok(Some("already in that mode".into())),
            }
            rt.lock().tool_state_template = None;
            if let Ok(info) = rt.info().await {
                rt.emit(ProjectEvent::ProjectChanged { info: Box::new(info) });
            }
            let ids: Vec<String> = rt.ws.deck().read_deck()?.slide_ids().map(|s| s.to_string()).collect();
            rt.deck_changed(ids, vec![], "user");
            Ok(None)
        })
    }

    // ---- skills, layout -----------------------------------------------------------------------

    pub fn skills_list(&self) -> Vec<SkillView> {
        let disabled = self.lock().disabled_skills.clone();
        session::discover_skills(&self.home, &self.ws.project.root)
            .list()
            .into_iter()
            .map(|s| SkillView {
                enabled: !disabled.contains(&s.name),
                source: serde_json::to_value(s.source)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                name: s.name,
                description: s.description,
                has_scripts: s.has_scripts,
            })
            .collect()
    }

    pub fn skill_set_enabled(&self, name: &str, enabled: bool) {
        {
            let mut st = self.lock();
            if enabled {
                st.disabled_skills.remove(name);
            } else {
                st.disabled_skills.insert(name.to_string());
            }
        }
        self.save_state();
    }

    pub fn layout_get(&self) -> Option<Value> {
        self.lock().layout.clone()
    }

    pub fn layout_set(&self, layout: Value) {
        self.lock().layout = Some(layout);
        self.save_state();
    }

    // ---- conversations and chat ---------------------------------------------------------------

    pub fn conversation_new(&self) -> ConversationSummary {
        let store = session::transcript_store(&self.home, &self.ws.project.root);
        let id = store.new_conversation_id();
        {
            let mut st = self.lock();
            st.conversations.insert(id.clone(), ConvState::default());
            st.active = id.clone();
        }
        self.save_state();
        self.conversations()
            .into_iter()
            .find(|c| c.id == id)
            .unwrap_or(ConversationSummary {
                id,
                title: "New conversation".into(),
                model: None,
                mode: Mode::Build,
                created_at: now_iso(),
                turns: 0,
                busy: false,
            })
    }

    pub fn conversation_select(&self, conversation: &str) -> R<()> {
        let mut st = self.lock();
        st.conversations
            .entry(conversation.to_string())
            .or_default();
        st.active = conversation.to_string();
        drop(st);
        self.save_state();
        Ok(())
    }

    pub fn conversation_transcript(&self, conversation: &str) -> R<Vec<Value>> {
        let store = session::transcript_store(&self.home, &self.ws.project.root);
        if !store.exists(conversation) {
            return Ok(Vec::new());
        }
        let events = store.read(conversation).map_err(internal)?;
        Ok(events
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect())
    }

    pub fn chat_set_mode(&self, conversation: &str, mode: Mode) {
        self.lock()
            .conversations
            .entry(conversation.to_string())
            .or_default()
            .mode = Some(to_agent_mode(mode));
        self.save_state();
    }

    pub fn chat_set_model(&self, conversation: &str, model: &str) {
        let mut st = self.lock();
        st.conversations
            .entry(conversation.to_string())
            .or_default()
            .model = Some(model.to_string());
        st.model = Some(model.to_string());
    }

    pub fn proposal_respond(&self, proposal_id: &str, decision: ProposalDecision) -> R<()> {
        let sender = self
            .lock()
            .pending
            .remove(proposal_id)
            .ok_or_else(|| IpcError::not_found("the proposal is already resolved"))?;
        sender
            .send(decision)
            .map_err(|_| IpcError::not_found("the turn that made this proposal has ended"))
    }

    pub fn chat_cancel(&self, conversation: &str) {
        let mut st = self.lock();
        if let Some(conv) = st.conversations.get_mut(conversation) {
            if let Some(handle) = conv.cancel.take() {
                handle.abort();
            }
            conv.busy = false;
        }
        // Dropping the senders rejects every pending proposal.
        st.pending.clear();
    }

    fn proposal_view(
        &self,
        request: &ApprovalRequest,
        conversation: &str,
        turn: &str,
    ) -> ProposalView {
        let mut view = ProposalView {
            id: request.id.clone(),
            conversation: conversation.to_string(),
            turn: turn.to_string(),
            kind: request.kind.clone(),
            summary: request.summary.clone(),
            reason: Some(request.reason.replace('_', "-")),
            slides: Vec::new(),
            outline: None,
            code: None,
            script: None,
        };
        match &request.change {
            Some(Change::SlideCreate { slide, .. }) => view.slides.push(ProposalSlide {
                slide_id: slide.id.to_string(),
                before: None,
                after: Some((**slide).clone()),
                before_png: None,
                after_png: None,
                text_diff: request.diff.clone(),
            }),
            Some(Change::SlideUpdate { before, after, .. }) => view.slides.push(ProposalSlide {
                slide_id: after.id.to_string(),
                before: Some((**before).clone()),
                after: Some((**after).clone()),
                before_png: self
                    .lock()
                    .thumbs
                    .get(after.id.as_str())
                    .map(|t| t.0.clone()),
                after_png: None,
                text_diff: request.diff.clone(),
            }),
            Some(Change::SlideDelete { before }) => view.slides.push(ProposalSlide {
                slide_id: before.id.to_string(),
                before: Some((**before).clone()),
                after: None,
                before_png: self
                    .lock()
                    .thumbs
                    .get(before.id.as_str())
                    .map(|t| t.0.clone()),
                after_png: None,
                text_diff: request.diff.clone(),
            }),
            Some(Change::SlideMove { id, .. }) => view.slides.push(ProposalSlide {
                slide_id: id.to_string(),
                before: None,
                after: None,
                before_png: None,
                after_png: None,
                text_diff: request.summary.clone(),
            }),
            Some(Change::Outline { rows }) => {
                view.outline = Some(
                    rows.iter()
                        .map(|r| OutlineRow {
                            slide_id: r.slide_id.clone(),
                            title: r.title.clone(),
                            message: r.message.clone(),
                            layout: r.layout.clone(),
                            sources: r.sources.clone(),
                        })
                        .collect(),
                )
            }
            Some(Change::CodeEdit {
                path,
                original,
                proposed,
            }) => {
                view.code = Some(ProposalCode {
                    path: path.clone(),
                    original: original.clone(),
                    proposed: proposed.clone(),
                })
            }
            Some(Change::ModeSwitch { .. }) | None => {}
        }
        if request.kind == "skill-script" {
            view.script = Some(ProposalScript {
                skill: None,
                path: request.summary.clone(),
                args: Vec::new(),
            });
        }
        view
    }

    /// Start a turn. Returns its id at once; progress arrives as events.
    pub async fn chat_send(
        self: &Arc<Self>,
        conversation: &str,
        text: &str,
        attachments: Option<ChatAttachments>,
    ) -> R<String> {
        let (mode, model_name) = {
            let mut st = self.lock();
            let fallback = st.model.clone();
            let conv = st
                .conversations
                .entry(conversation.to_string())
                .or_default();
            if conv.busy {
                return Err(IpcError::invalid("the conversation is busy"));
            }
            conv.busy = true;
            (
                conv.mode.unwrap_or(AgentMode::Build),
                conv.model.clone().or(fallback),
            )
        };
        let release = |rt: &ProjectRuntime| {
            if let Some(conv) = rt.lock().conversations.get_mut(conversation) {
                conv.busy = false;
                conv.cancel = None;
            }
        };
        let resolved = match &self.model_factory {
            Some(factory) => factory(model_name.as_deref()).await,
            None => {
                session::resolve_model(
                    &self.home,
                    &crate::config::load_global(&self.home).unwrap_or_default(),
                    model_name.as_deref(),
                    false,
                )
                .await
            }
        };
        let resolved = match resolved {
            Ok(r) => r,
            Err(e) => {
                release(self);
                return Err(IpcError::invalid(format!("{e:#}")));
            }
        };
        let policy = match self.policy() {
            Ok(p) => p,
            Err(e) => {
                release(self);
                return Err(e);
            }
        };
        let store = session::transcript_store(&self.home, &self.ws.project.root);
        let turns = if store.exists(conversation) {
            store
                .read(conversation)
                .map(|e| e.iter().filter(|x| x.type_name() == "user").count())
                .unwrap_or(0)
        } else {
            0
        };
        let turn_id = format!("{conversation}-{}", turns + 1);
        let approver = Arc::new(BridgeApprover {
            rt: self.clone(),
            conversation: conversation.to_string(),
            turn: turn_id.clone(),
        });
        let max_steps = policy.max_steps;
        let mut agent = match session::build_agent(
            self.ws.clone(),
            &resolved,
            policy,
            approver,
            mode,
            conversation,
            max_steps,
        ) {
            Ok(a) => a,
            Err(e) => {
                release(self);
                return Err(internal(e));
            }
        };
        agent.state.disabled_skills = self.lock().disabled_skills.clone();
        let mut message = text.to_string();
        if let Some(att) = attachments {
            if !att.slides.is_empty() {
                message.push_str(&format!(
                    "\n\n[Slides the user selected: {}]",
                    att.slides.join(", ")
                ));
            }
            if !att.cites.is_empty() {
                message.push_str(&format!(
                    "\n\n[Reference passages the user attached: {}]",
                    att.cites.join(", ")
                ));
            }
        }
        let (rt_text, conv_text, turn_text) =
            (self.clone(), conversation.to_string(), turn_id.clone());
        agent.on_text = Some(Arc::new(move |delta: &str| {
            rt_text.emit(ProjectEvent::AgentText {
                conversation: conv_text.clone(),
                turn: turn_text.clone(),
                delta: delta.to_string(),
            })
        }));
        let (rt_ev, conv_ev, turn_ev, window) = (
            self.clone(),
            conversation.to_string(),
            turn_id.clone(),
            resolved.caps.context_window,
        );
        agent.on_event = Some(Arc::new(move |event: &wb_transcript::Event| match event {
            wb_transcript::Event::ToolCall {
                id,
                name,
                arguments,
                ..
            } => rt_ev.emit(ProjectEvent::AgentToolCall {
                conversation: conv_ev.clone(),
                turn: turn_ev.clone(),
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            wb_transcript::Event::ToolResult { id, ok, result, .. } => {
                let summary: String = result.to_string().chars().take(240).collect();
                rt_ev.emit(ProjectEvent::AgentToolResult {
                    conversation: conv_ev.clone(),
                    turn: turn_ev.clone(),
                    id: id.clone(),
                    ok: *ok,
                    summary,
                    image: None,
                })
            }
            wb_transcript::Event::Usage {
                input_tokens,
                output_tokens,
                ..
            } => rt_ev.emit(ProjectEvent::Usage {
                conversation: conv_ev.clone(),
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
                context_window: window,
                context_used: *input_tokens,
            }),
            _ => {}
        }));
        self.emit(ProjectEvent::TurnStarted {
            conversation: conversation.to_string(),
            turn: turn_id.clone(),
        });
        let (rt, conv, turn) = (self.clone(), conversation.to_string(), turn_id.clone());
        let handle = tokio::spawn(async move {
            let outcome = agent.run_turn(&message).await;
            if let Some(c) = rt.lock().conversations.get_mut(&conv) {
                c.busy = false;
                c.cancel = None;
            }
            match outcome {
                Ok(summary) => {
                    if !summary.slides_changed.is_empty() {
                        let removed: Vec<String> = summary
                            .slides_changed
                            .iter()
                            .filter(|s| !rt.ws.deck().slide_path_exists(s))
                            .cloned()
                            .collect();
                        let changed: Vec<String> = summary
                            .slides_changed
                            .iter()
                            .filter(|s| rt.ws.deck().slide_path_exists(s))
                            .cloned()
                            .collect();
                        rt.deck_changed(changed, removed, "agent");
                    }
                    rt.emit(ProjectEvent::TurnDone {
                        conversation: conv,
                        turn,
                        summary: TurnSummaryView {
                            text: summary.text,
                            slides_changed: summary.slides_changed,
                            exports: summary.exports,
                            open_issues: summary.open_issues,
                        },
                    });
                }
                Err(e) => rt.emit(ProjectEvent::TurnError {
                    conversation: conv,
                    turn,
                    message: format!("{e:#}"),
                }),
            }
        });
        if let Some(c) = self.lock().conversations.get_mut(conversation) {
            c.cancel = Some(handle.abort_handle());
        }
        Ok(turn_id)
    }
}

fn empty_deck() -> present_protocol::deck::Deck {
    present_protocol::deck::Deck {
        schema_version: present_protocol::SCHEMA_VERSION,
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

fn locate(deck: &present_protocol::deck::Deck, id: &SlideId) -> (u32, String) {
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

pub fn to_ui_mode(mode: AgentMode) -> Mode {
    match mode {
        AgentMode::Outline => Mode::Outline,
        AgentMode::Build => Mode::Build,
        AgentMode::Polish => Mode::Polish,
    }
}

pub fn to_agent_mode(mode: Mode) -> AgentMode {
    match mode {
        Mode::Outline => AgentMode::Outline,
        Mode::Build => AgentMode::Build,
        Mode::Polish => AgentMode::Polish,
    }
}
