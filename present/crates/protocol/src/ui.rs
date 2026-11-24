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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum KernelState {
    Stopped,
    Starting,
    Ready,
    Error,
}

/// A deck of the session: what `deck_open` returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct DeckInfo {
    /// The deck's folder name under `decks/`.
    pub slug: String,
    pub title: String,
    pub path_abs: String,
    pub template_name: String,
    pub deck_mode: DeckMode,
    pub slides: u32,
    pub kernel: KernelState,
    /// How the deck kernel process is kept off the network: `bwrap`, `sandbox-exec`, or
    /// `none` (no isolation is available on this machine).
    pub kernel_network: String,
    pub engines: Vec<EngineInfo>,
    pub sandbox: Option<SandboxInfo>,
    pub embedding: Option<EmbeddingInfo>,
    pub template: TemplateDescription,
    pub deck: Deck,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ExportView {
    pub id: String,
    pub format: String,
    pub preset: Option<String>,
    pub path: String,
    /// The session checkpoint taken when the export was made (the state it was built from).
    pub checkpoint: String,
    pub created_at: String,
    pub errors: u32,
    pub warnings: u32,
    pub engine: Option<String>,
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

/// Events of one deck, pushed on the session channel inside a `deck` session event
/// (IPC.md section 7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeckEvent {
    DeckChanged {
        deck: Box<Deck>,
        changed: Vec<String>,
        removed: Vec<String>,
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
    /// The deck's mode, template, or title changed.
    InfoChanged {
        info: Box<DeckInfo>,
    },
}
