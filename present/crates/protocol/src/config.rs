//! Configuration and project files (PROTOCOL 4): the global `config.toml` (4.1), the
//! project's `.present/project.toml` (4.2), `references/manifest.toml` (4.3), and the
//! snapshot store's manifest and log record (4.4).
//!
//! Times are RFC 3339 UTC strings, kept as written.

use crate::cite::RefKind;
use crate::deck::DeckMode;
use crate::ids::{RefId, SnapshotId};
use crate::kernel::Severity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

// ---- global config.toml (4.1) ----

/// `~/.present/config.toml` (macOS, Linux) or `%APPDATA%\Present\config.toml` (Windows).
///
/// There are no network settings: the app reaches model endpoints directly, through the
/// network gate, without a proxy or extra trust roots. A `[network]` table left in an older
/// file is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct GlobalConfig {
    /// Interpreter that hosts `present_kernel`; second in the resolution order of
    /// PROTOCOL 1.1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_python: Option<String>,
    pub defaults: Defaults,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelConfig>,
    pub embedding: EmbeddingConfig,
    pub render: RenderConfig,
    pub ui: UiConfig,
}

impl GlobalConfig {
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Defaults {
    /// `"<provider name>/<model id>"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
pub enum ProviderKind {
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    XApiKey,
    Bearer,
    None,
}

/// Where a provider runs, shown in the chat header and written to the audit log.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Location {
    Internal,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Default: `x-api-key` for `anthropic`; see [`ProviderConfig::effective_auth`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthKind>,
    /// `env:<VAR>`; see [`KeySource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Extra headers, sent on every request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

impl ProviderConfig {
    /// The configured auth scheme, or the kind's default. PROTOCOL 4.1 states the default
    /// only for `anthropic` (`x-api-key`); `bearer` is assumed for `openai-compatible`.
    pub fn effective_auth(&self) -> AuthKind {
        self.auth.unwrap_or(match self.kind {
            ProviderKind::Anthropic => AuthKind::XApiKey,
            ProviderKind::OpenAiCompatible => AuthKind::Bearer,
        })
    }

    /// The configured location; a provider that does not say is treated as external.
    pub fn effective_location(&self) -> Location {
        self.location.unwrap_or(Location::External)
    }

    /// The parsed `key`, if set and well-formed.
    pub fn key_source(&self) -> Option<KeySource> {
        KeySource::parse(self.key.as_deref()?)
    }
}

/// Where a provider key is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// `env:<VAR>`: a variable in the process environment or in the app home's `.env` file.
    Env(String),
}

impl KeySource {
    pub fn parse(s: &str) -> Option<KeySource> {
        let (scheme, rest) = s.split_once(':')?;
        if rest.is_empty() {
            return None;
        }
        match scheme {
            "env" => Some(KeySource::Env(rest.to_string())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelConfig {
    pub provider: String,
    /// Sent verbatim; may be a gateway alias.
    pub id: String,
    /// Optional override of the probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Optional override of the probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct EmbeddingConfig {
    pub kind: EmbeddingSource,
    /// Provider name, when `kind = "endpoint"` (OpenAI-compatible `/embeddings`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

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
pub enum EmbeddingSource {
    #[default]
    Bundled,
    Endpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RenderConfig {
    pub engine: RenderEngine,
    /// Optional explicit `soffice` path; empty for discovery.
    pub libreoffice_path: String,
}

/// Fidelity engine selection; also the kernel's `--engine` argument.
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
pub enum RenderEngine {
    #[default]
    Auto,
    Powerpoint,
    Libreoffice,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct UiConfig {
    pub theme: UiTheme,
}

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
pub enum UiTheme {
    #[default]
    System,
    Light,
    Dark,
}

// ---- project.toml (4.2) ----

/// `<project>/.present/project.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ProjectConfig {
    pub project: ProjectSection,
    pub policy: PolicyConfig,
    pub contract: ContractConfig,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub export_presets: Vec<ExportPreset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ProjectSection {
    pub name: String,
    pub created: String,
}

/// How slide edits are gated.
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
#[serde(rename_all = "kebab-case")]
pub enum EditPolicy {
    #[default]
    Review,
    AutoNewSlides,
    Auto,
}

/// `ask | auto | deny`.
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
pub enum Gate {
    #[default]
    Ask,
    Auto,
    Deny,
}

/// `allow | deny`.
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
pub enum Permission {
    #[default]
    Allow,
    Deny,
}

/// `[policy]`. Defaults are the values of the PROTOCOL 4.2 example.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(default)]
pub struct PolicyConfig {
    pub edits: EditPolicy,
    /// Builds and generated slides.
    pub code: Gate,
    pub skills_scripts: Gate,
    /// Switching to code generation mode.
    pub code_mode: Permission,
    pub mcp: Gate,
    pub export: Gate,
    pub max_steps: u32,
    pub build_timeout_s: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        PolicyConfig {
            edits: EditPolicy::Review,
            code: Gate::Ask,
            skills_scripts: Gate::Ask,
            code_mode: Permission::Allow,
            mcp: Gate::Ask,
            export: Gate::Auto,
            max_steps: 60,
            build_timeout_s: 60,
        }
    }
}

/// `[contract]`: limits plus `"<rule id>" = "<severity>"` overrides (PROTOCOL 7); omitted
/// rules use their default severity. Limit defaults are the values of the PROTOCOL 4.2
/// example.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(default)]
pub struct ContractConfig {
    pub max_bullets: u32,
    pub max_words: u32,
    pub min_font_pt: u32,
    pub allow_free_elements: bool,
    /// Rule id to severity.
    #[serde(flatten)]
    pub severities: BTreeMap<String, Severity>,
}

impl Default for ContractConfig {
    fn default() -> Self {
        ContractConfig {
            max_bullets: 6,
            max_words: 70,
            min_font_pt: 12,
            allow_free_elements: false,
            severities: BTreeMap::new(),
        }
    }
}

/// `[[export_presets]]`. `file` may use `{project}` and `{date}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct ExportPreset {
    pub name: String,
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dpi: Option<u32>,
    pub file: String,
}

// ---- references/manifest.toml (4.3) ----

/// `references/manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RefsManifest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<RefEntry>,
}

impl RefsManifest {
    pub fn get(&self, id: &RefId) -> Option<&RefEntry> {
        self.refs.iter().find(|r| &r.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefEntry {
    pub id: RefId,
    /// `references/<first 12 hex of sha256>.<ext>`; versions are separate files.
    pub file: String,
    pub kind: RefKind,
    pub title: String,
    pub original_path: String,
    pub imported: String,
    /// First 12 hex digits of the file's SHA-256.
    pub version: String,
    #[serde(default)]
    pub previous_versions: Vec<String>,
    /// Reference id of the email this is an attachment of; empty otherwise.
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub source: RefSource,
    #[serde(default)]
    pub analyst: AnalystOrigin,
}

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
pub enum RefSource {
    #[default]
    File,
    Analyst,
}

/// Provenance of a reference imported from Personal Analyst; all empty for `source = "file"`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct AnalystOrigin {
    pub repo: String,
    pub item: String,
    pub run_id: String,
}

// ---- snapshot store (4.4) ----

/// `.present/history/snapshots/<snapshot_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct SnapshotManifest {
    pub snapshot_id: SnapshotId,
    pub created: String,
    /// `null` for the first snapshot of a project.
    #[serde(default)]
    pub parent: Option<SnapshotId>,
    pub mode: DeckMode,
    /// Deck files (slide JSON, `deck.json`, `build.py` and modules) by blob hash.
    pub files: Vec<SnapshotFile>,
    /// SHA-256 of every asset the deck uses; listed, never copied.
    pub assets: Vec<String>,
    pub refs: Vec<SnapshotRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct SnapshotFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct SnapshotRef {
    pub id: RefId,
    pub version: String,
}

/// One line of `.present/history/log.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct SnapshotLogRecord {
    pub ts: String,
    pub snapshot_id: SnapshotId,
    #[serde(default)]
    pub parent: Option<SnapshotId>,
    pub reason: SnapshotReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotReason {
    Turn,
    ModeSwitch,
    Export,
    Restore,
    Manual,
}

impl SnapshotReason {
    /// Pruning never removes these (PROTOCOL 4.4).
    pub fn is_pinned(self) -> bool {
        matches!(self, SnapshotReason::ModeSwitch | SnapshotReason::Export)
    }
}
