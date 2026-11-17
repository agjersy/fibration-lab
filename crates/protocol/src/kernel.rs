//! Deck kernel JSON-RPC: envelope, error codes, method parameters and results,
//! diagnostics, and notifications (PROTOCOL 1).
//!
//! All paths are strings relative to the project root with forward slashes unless the
//! field name ends in `_abs`. Each method is a marker type implementing [`Method`], which
//! ties the wire name to its parameter and result types.

use crate::deck::{ChartSource, Size, Table, TransformStep};
use crate::ids::{RefId, SlideId, SnapshotId};
use crate::template::{Autofit, Insets, TemplateDescription, TemplateSidecar};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

pub use crate::cite::RefKind;
pub use crate::rpc::{
    codes, BuildErrorData, ErrorData, Incoming, RawNotification, RenderErrorData, Request,
    RequestId, Response, RpcError, SchemaErrorData, SchemaErrorItem, TransformErrorData,
};

/// A kernel method: its wire name and the types of `params` and `result`.
pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;

    /// A request envelope for this method.
    fn request(
        id: impl Into<RequestId>,
        params: &Self::Params,
    ) -> Result<Request, serde_json::Error> {
        Ok(Request::new(id, Self::NAME, serde_json::to_value(params)?))
    }
}

macro_rules! methods {
    ($($(#[$meta:meta])* $ty:ident = $name:literal : $params:ty => $result:ty;)*) => {
        $(
            $(#[$meta])*
            #[doc = concat!("`", $name, "`")]
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $ty;

            impl Method for $ty {
                const NAME: &'static str = $name;
                type Params = $params;
                type Result = $result;
            }
        )*

        /// Wire names of every method in PROTOCOL 1.3, in document order.
        pub const METHOD_NAMES: &[&str] = &[$($name),*];
    };
}

methods! {
    KernelHello = "kernel.hello": HelloParams => HelloResult;
    KernelShutdown = "kernel.shutdown": EmptyParams => ();
    TemplateInspect = "template.inspect": TemplatePathParams => TemplateDescription;
    TemplateThumbnails = "template.thumbnails": TemplateThumbnailsParams => Vec<LayoutThumbnail>;
    TemplateSidecarMethod = "template.sidecar": TemplatePathParams => Option<TemplateSidecar>;
    DeckValidate = "deck.validate": DeckValidateParams => Vec<Diagnostic>;
    DeckRender = "deck.render": DeckRenderParams => DeckRenderResult;
    DeckEject = "deck.eject": DeckEjectParams => DeckEjectResult;
    SlideGenerate = "slide.generate": SlideGenerateParams => SlideGenerateResult;
    DeckBuild = "deck.build": DeckBuildParams => DeckBuildResult;
    RenderFidelity = "render.fidelity": RenderFidelityParams => RenderFidelityResult;
    CheckRun = "check.run": CheckRunParams => Vec<Diagnostic>;
    TextMeasure = "text.measure": TextMeasureParams => TextMeasureResult;
    RefsIngest = "refs.ingest": RefsIngestParams => RefsIngestResult;
    RefsRead = "refs.read": RefsReadParams => RefsReadResult;
    RefsTable = "refs.table": RefsTableParams => Table;
    RefsPageImage = "refs.page_image": RefsPageImageParams => PngResult;
    EmbedTexts = "embed.texts": EmbedTextsParams => EmbedTextsResult;
    ChartCompute = "chart.compute": ChartComputeParams => ChartComputeResult;
    ExportOutlineMd = "export.outline_md": ExportParams => OutResult;
    ExportNotesDocx = "export.notes_docx": ExportParams => OutResult;
    AnalystScan = "analyst.scan": AnalystScanParams => AnalystScanResult;
    AnalystImport = "analyst.import": AnalystImportParams => AnalystImportResult;
}

/// `{}`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct EmptyParams {}

// ---- lifecycle ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HelloParams {
    pub core_version: String,
    /// [`crate::PROTOCOL_VERSION`].
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct HelloResult {
    pub kernel_version: String,
    pub protocol_version: u32,
    pub schema_version: u32,
    pub os: Os,
    pub python: String,
    /// The fixed bundled package set with versions.
    pub packages: BTreeMap<String, String>,
    pub sandbox: SandboxInfo,
    pub engines: Vec<EngineInfo>,
    pub embedding: EmbeddingInfo,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct SandboxInfo {
    pub kind: SandboxKind,
    pub available: bool,
    pub detail: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxKind {
    SandboxExec,
    Appcontainer,
    Bwrap,
    Limits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct EngineInfo {
    pub kind: EngineKind,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path_abs: Option<String>,
    pub detail: String,
}

/// A fidelity engine.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    Powerpoint,
    Libreoffice,
}

/// The `engine` argument of `render.fidelity`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    TS,
)]
#[serde(rename_all = "lowercase")]
pub enum EngineChoice {
    #[default]
    Auto,
    Powerpoint,
    Libreoffice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct EmbeddingInfo {
    pub kind: EmbeddingKind,
    pub model: String,
    pub dims: u32,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingKind {
    Bundled,
    Endpoint,
    None,
}

// ---- templates ----

/// Parameters of `template.inspect` and `template.sidecar`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplatePathParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateThumbnailsParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct LayoutThumbnail {
    pub layout: String,
    /// Project-relative path under `.present/cache/`.
    pub png: String,
}

// ---- deck (structured mode) ----

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeckValidateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeckRenderParams {
    pub out: String,
    pub snapshot_id: SnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slides: Option<Vec<SlideId>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeckRenderResult {
    pub out: String,
    pub sha256: String,
    pub element_map: Vec<ElementMapEntry>,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeckEjectParams {
    pub out_dir: String,
}

/// PROTOCOL 5.4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeckEjectResult {
    pub script: String,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SlideGenerateParams {
    pub slide_id: SlideId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SlideGenerateResult {
    pub out: String,
    pub stdout: String,
    pub stderr: String,
}

// ---- deck (code mode) ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeckBuildParams {
    pub script: String,
    pub out: String,
    pub snapshot_id: SnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeckBuildResult {
    pub out: String,
    pub sha256: String,
    pub element_map: Vec<ElementMapEntry>,
    pub stdout: String,
    pub stderr: String,
    pub warnings: Vec<Diagnostic>,
}

/// Where one document element landed in the exported file.
///
/// The ids are plain strings so that a map from a code-mode build, whose slide ids the
/// SDK derives, never fails to parse; they match the `SlideId` and `ElementId` patterns
/// in practice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct ElementMapEntry {
    pub slide_id: String,
    pub element_id: String,
    pub slide_index: u32,
    pub shape_id: u32,
    pub kind: String,
}

// ---- rendering ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenderFidelityParams {
    pub pptx: String,
    pub format: FidelityFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slides: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<EngineChoice>,
    pub out_dir: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum FidelityFormat {
    Png,
    Pdf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenderFidelityResult {
    pub engine: EngineKind,
    pub files: Vec<String>,
}

// ---- checker ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckRunParams {
    pub pptx: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slides: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextMeasureParams {
    pub font_family: String,
    pub size_pt: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    pub paragraphs: Vec<String>,
    /// The text box, EMU.
    pub r#box: Size,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insets: Option<Insets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_spacing: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofit: Option<Autofit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextMeasureResult {
    pub fits: bool,
    pub lines: u32,
    /// Height of the laid-out text, EMU.
    pub height: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shrink_to_pt: Option<f64>,
    pub font_found: bool,
}

// ---- diagnostics (1.5) ----

/// One finding of the validator, renderer, build, or checker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct Diagnostic {
    /// A PROTOCOL 7 rule id, or `"schema"`, `"render"`, `"build"`.
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slide_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub element_id: Option<String>,
    /// 0-based position of the slide in the deck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slide_index: Option<u32>,
    /// JSON pointer into the slide document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fix_hint: Option<String>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

// ---- references ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefsIngestParams {
    /// Path under `references/` (already copied by the core).
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefsIngestResult {
    pub ref_id: RefId,
    pub kind: RefKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    pub outline: Vec<OutlineNode>,
    pub chunks: Vec<Chunk>,
    pub tables: Vec<TableRef>,
    pub images: Vec<ImageRef>,
    /// Reference ids of child references (email attachments).
    pub children: Vec<String>,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct OutlineNode {
    pub title: String,
    pub loc: String,
    pub children: Vec<OutlineNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct Chunk {
    pub loc: String,
    pub heading_path: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct TableRef {
    pub loc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    pub rows: u32,
    pub cols: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct ImageRef {
    pub loc: String,
    pub png: String,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub caption: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefsReadParams {
    pub ref_id: RefId,
    pub loc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefsReadResult {
    pub text: String,
    pub loc: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefsTableParams {
    pub ref_id: RefId,
    pub loc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefsPageImageParams {
    pub ref_id: RefId,
    pub page: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PngResult {
    pub png: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmbedTextsParams {
    pub texts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmbedTextsResult {
    pub vectors: Vec<Vec<f32>>,
}

// ---- data ----

/// PROTOCOL 2.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChartComputeParams {
    pub source: ChartSource,
    pub steps: Vec<TransformStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct ChartComputeResult {
    pub table: Table,
    /// The table after each step, for the source lens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub step_tables: Option<Vec<Table>>,
}

// ---- exports derived from the document ----

/// Parameters of `export.outline_md` and `export.notes_docx`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExportParams {
    pub deck: String,
    pub out: String,
    pub include_sources: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OutResult {
    pub out: String,
}

// ---- Personal Analyst ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnalystScanParams {
    pub repo_abs: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct AnalystScanResult {
    pub figures: Vec<AnalystItem>,
    pub tables: Vec<AnalystItem>,
}

/// `AnalystFigure` / `AnalystTable` of `analyst.scan`.
///
/// PROTOCOL 1.3 names these types without defining them. Only the fields the rest of the
/// contract depends on are typed: `item` is what `analyst.import` takes, and `run_id` and
/// `name` form the citation location `run:<run_id>/<name>`. Anything else the kernel
/// sends is kept in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct AnalystItem {
    pub item: String,
    pub run_id: String,
    pub name: String,
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: BTreeMap<String, Value>,
}

/// Type alias matching the PROTOCOL name.
pub type AnalystFigure = AnalystItem;
/// Type alias matching the PROTOCOL name.
pub type AnalystTable = AnalystItem;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnalystImportParams {
    pub repo_abs: String,
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnalystImportResult {
    pub ref_id: RefId,
}

// ---- notifications (kernel -> core) ----

/// A typed notification: `{"method": ..., "params": ...}` of the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "method", content = "params")]
pub enum Notification {
    #[serde(rename = "task.progress")]
    TaskProgress(TaskProgress),
    #[serde(rename = "log")]
    Log(LogMessage),
}

impl Notification {
    /// Types a raw notification; `None` for a method this version does not know.
    pub fn from_raw(raw: &RawNotification) -> Option<Notification> {
        let params = raw.params.clone().unwrap_or(Value::Null);
        match raw.method.as_str() {
            "task.progress" => serde_json::from_value(params)
                .ok()
                .map(Notification::TaskProgress),
            "log" => serde_json::from_value(params).ok().map(Notification::Log),
            _ => None,
        }
    }
}

/// Progress of a long operation (ingest, render, build), carrying the request id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct TaskProgress {
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fraction: Option<f64>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct LogMessage {
    pub level: LogLevel,
    pub message: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}
