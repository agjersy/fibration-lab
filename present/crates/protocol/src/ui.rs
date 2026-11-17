//! Desktop IPC types (IPC.md sections 2 and 3). These cross the boundary between the Rust
//! core and the React renderer and are exported to TypeScript with `ts-rs`.

use crate::cite::RefKind;
use crate::deck::{Deck, DeckMode, Slide, Table};
use crate::kernel::{Diagnostic, EmbeddingInfo, EngineInfo, SandboxInfo};
use crate::template::TemplateDescription;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Structured error returned by every command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct IpcError {
    pub code: IpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "unknown")]
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum IpcErrorCode {
    NotFound,
    Conflict,
    PolicyDenied,
    KernelOffline,
    EngineUnavailable,
    SandboxUnavailable,
    Invalid,
    Io,
    Internal,
}

impl IpcError {
    pub fn new(code: IpcErrorCode, message: impl Into<String>) -> IpcError {
        IpcError {
            code,
            message: message.into(),
            detail: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> IpcError {
        IpcError::new(IpcErrorCode::Invalid, message)
    }
    pub fn not_found(message: impl Into<String>) -> IpcError {
        IpcError::new(IpcErrorCode::NotFound, message)
    }
    pub fn internal(message: impl Into<String>) -> IpcError {
        IpcError::new(IpcErrorCode::Internal, message)
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for IpcError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Outline,
    Build,
    Polish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum KernelState {
    Stopped,
    Starting,
    Ready,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct AppInfo {
    pub version: String,
    pub home_abs: String,
    pub platform: Platform,
    pub dev_mode: bool,
    pub engines: Vec<EngineInfo>,
    pub sandbox: Option<SandboxInfo>,
    pub embedding: Option<EmbeddingInfo>,
    /// How the deck kernel process is kept off the network: `bwrap`, `sandbox-exec`, or
    /// `none` (no isolation is available on this machine).
    pub kernel_network: String,
    /// Why there is no isolation, when `kernel_network` is `none`.
    pub kernel_network_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub path_abs: String,
    pub template_name: String,
    pub deck_mode: DeckMode,
    pub slides: u32,
    pub model: Option<String>,
    pub created_at: String,
    pub last_opened_at: String,
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PolicyView {
    /// `review` | `auto-new-slides` | `auto`.
    pub edits: String,
    pub code: String,
    pub skills_scripts: String,
    pub code_mode: String,
    pub mcp: String,
    pub export: String,
    pub max_steps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
pub struct PolicyPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edits: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub skills_scripts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub export: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_steps: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub model: Option<String>,
    pub mode: Mode,
    pub created_at: String,
    pub turns: u32,
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectInfo {
    #[serde(flatten)]
    pub summary: ProjectSummary,
    pub policy: PolicyView,
    pub kernel: KernelState,
    pub conversations: Vec<ConversationSummary>,
    pub active_conversation: String,
    pub template: TemplateDescription,
    pub deck: Deck,
    pub head_snapshot: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ThumbKind {
    Fidelity,
    Preview,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Thumb {
    pub kind: ThumbKind,
    pub path: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
pub struct CheckCounts {
    pub errors: u32,
    pub warnings: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
pub struct NumberCounts {
    pub green: u32,
    pub amber: u32,
    pub red: u32,
    pub grey: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SlideView {
    pub slide: Slide,
    /// Hash of the slide file, to pass back as `expected_hash`.
    pub hash: String,
    pub index: u32,
    pub section: String,
    pub thumb: Thumb,
    pub checks: CheckCounts,
    pub numbers: NumberCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct DeckView {
    pub deck: Deck,
    pub slides: Vec<SlideView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RefView {
    pub id: String,
    pub version: String,
    pub kind: RefKind,
    pub title: String,
    pub size_bytes: u64,
    pub pages: Option<u32>,
    /// `queued` | `extracting` | `embedding` | `ready` | `error`.
    pub status: String,
    pub error: Option<String>,
    pub cited_by: Vec<String>,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SourcePassage {
    pub cite: String,
    pub ref_title: String,
    pub kind: RefKind,
    pub text: Option<String>,
    pub highlight: Option<(u32, u32)>,
    pub page_png: Option<String>,
    pub table: Option<Table>,
    pub highlight_cells: Option<(u32, u32, u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SearchHit {
    pub cite: String,
    pub title: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutlineRow {
    pub slide_id: Option<String>,
    pub title: String,
    pub message: String,
    pub layout: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ProposalSlide {
    pub slide_id: String,
    pub before: Option<Slide>,
    pub after: Option<Slide>,
    pub before_png: Option<String>,
    pub after_png: Option<String>,
    pub text_diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProposalCode {
    pub path: String,
    pub original: Option<String>,
    pub proposed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProposalScript {
    pub skill: Option<String>,
    pub path: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ProposalView {
    pub id: String,
    pub conversation: String,
    pub turn: String,
    /// `slide-create` | `slide-update` | `slide-delete` | `slide-move` | `outline` | `code-edit`
    /// | `build` | `skill-script` | `mode-switch` | `export`.
    pub kind: String,
    pub summary: String,
    /// `policy` | `out-of-scope` | `always-ask`.
    pub reason: Option<String>,
    pub slides: Vec<ProposalSlide>,
    pub outline: Option<Vec<OutlineRow>>,
    pub code: Option<ProposalCode>,
    pub script: Option<ProposalScript>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ProposalDecision {
    Accept,
    AcceptModified {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        slides: Option<Vec<Slide>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        outline: Option<Vec<OutlineRow>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        code: Option<String>,
    },
    Reject {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ExportView {
    pub id: String,
    pub format: String,
    pub preset: Option<String>,
    pub path: String,
    pub snapshot_id: String,
    pub created_at: String,
    pub errors: u32,
    pub warnings: u32,
    pub engine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SnapshotView {
    pub id: String,
    pub created: String,
    pub reason: String,
    pub label: Option<String>,
    pub turn: Option<String>,
    pub slides: u32,
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CapabilitiesView {
    pub context_window: u32,
    /// `native` | `none` (a model without native tool calling cannot be used).
    pub tools: String,
    pub vision: bool,
    pub documents: bool,
    pub caching: bool,
    pub probed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelEntry {
    /// `provider/id`.
    pub name: String,
    pub provider: String,
    /// `internal` | `external`.
    pub location: String,
    pub capabilities: Option<CapabilitiesView>,
    pub degraded: bool,
    pub degraded_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SkillView {
    pub name: String,
    pub description: String,
    /// `builtin` | `user` | `project` | `managed`.
    pub source: String,
    pub has_scripts: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AnalystItem {
    pub id: String,
    /// `figure` | `table`.
    pub kind: String,
    pub title: String,
    pub run_id: String,
    pub script: String,
    pub args: Vec<String>,
    pub created: String,
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AssetView {
    pub path: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WriteResult {
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
pub struct ChatAttachments {
    #[serde(default)]
    pub slides: Vec<String>,
    #[serde(default)]
    pub cites: Vec<String>,
    #[serde(default)]
    pub images_abs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TurnSummaryView {
    pub text: String,
    pub slides_changed: Vec<String>,
    pub exports: Vec<String>,
    pub open_issues: Vec<String>,
}

/// Events pushed on a project's channel (IPC.md section 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProjectEvent {
    AgentText {
        conversation: String,
        turn: String,
        delta: String,
    },
    AgentToolCall {
        conversation: String,
        turn: String,
        id: String,
        name: String,
        #[ts(type = "unknown")]
        arguments: Value,
    },
    AgentToolResult {
        conversation: String,
        turn: String,
        id: String,
        ok: bool,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        image: Option<String>,
    },
    Proposal {
        proposal: Box<ProposalView>,
    },
    ProposalResolved {
        proposal_id: String,
        decision: String,
        by: String,
    },
    TurnStarted {
        conversation: String,
        turn: String,
    },
    TurnDone {
        conversation: String,
        turn: String,
        summary: TurnSummaryView,
    },
    TurnError {
        conversation: String,
        turn: String,
        message: String,
    },
    Usage {
        conversation: String,
        input_tokens: u64,
        output_tokens: u64,
        context_window: u32,
        context_used: u64,
    },
    DeckChanged {
        deck: Box<Deck>,
        changed: Vec<String>,
        removed: Vec<String>,
        head_snapshot: Option<String>,
        by: String,
    },
    SlideView {
        view: Box<SlideView>,
    },
    Diagnostics {
        slides: Vec<String>,
        diagnostics: Vec<Diagnostic>,
    },
    RefsChanged {
        refs: Vec<RefView>,
    },
    BuildOutput {
        stream: String,
        chunk: String,
    },
    ExportDone {
        export: ExportView,
    },
    KernelState {
        state: KernelState,
        message: Option<String>,
    },
    TaskProgress {
        task_id: String,
        fraction: Option<f64>,
        message: String,
    },
    TaskDone {
        task_id: String,
        ok: bool,
        message: Option<String>,
    },
    ProjectChanged {
        info: Box<ProjectInfo>,
    },
}
