//! Deck files (PROTOCOL 4): the deck config `<deck>/.present/project.toml` (4.2) and
//! `references/manifest.toml` (4.3). Providers, models, and policy presets are in the one
//! app config (`~/.fibration/config.toml`), not here; undo is the repository's git checkpoints.
//!
//! Times are RFC 3339 UTC strings, kept as written.

use crate::cite::RefKind;
use crate::ids::RefId;
use crate::kernel::Severity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

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
    /// No edit is ever applied (the `read-only` preset).
    Deny,
}

/// `policy.preset`: `review` (edits reviewed, builds and skill scripts start on their own),
/// `auto` (edits inside the declared scope applied, outside it asked), `read-only` (no
/// edits, no switch to code generation mode; builds and skill scripts still run). It
/// expands to the detailed keys; a detailed key in the same table overrides it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyPreset {
    Review,
    Auto,
    ReadOnly,
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
///
/// The table is read by `wb-policy` (`PresentPolicyConfig`), which expands `preset`; the
/// values here are the effective ones. Keys of older files that no longer exist (`mcp`)
/// are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema, TS)]
pub struct PolicyConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preset: Option<PolicyPreset>,
    pub edits: EditPolicy,
    /// Builds and generated slides.
    pub code: Gate,
    pub skills_scripts: Gate,
    /// Switching to code generation mode.
    pub code_mode: Permission,
    pub export: Gate,
    pub max_steps: u32,
    pub build_timeout_s: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        PolicyConfig {
            preset: None,
            edits: EditPolicy::Review,
            code: Gate::Ask,
            skills_scripts: Gate::Ask,
            code_mode: Permission::Allow,
            export: Gate::Auto,
            max_steps: 60,
            build_timeout_s: 60,
        }
    }
}

impl<'de> Deserialize<'de> for PolicyConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use wb_policy::config::{AllowDeny, PresentEdits};
        use wb_policy::{Level, Preset};
        let gate = |l: Level| match l {
            Level::Ask => Gate::Ask,
            Level::Auto => Gate::Auto,
            Level::Deny => Gate::Deny,
        };
        let c = wb_policy::PresentPolicyConfig::deserialize(d)?;
        Ok(PolicyConfig {
            preset: c.preset.map(|p| match p {
                Preset::Review => PolicyPreset::Review,
                Preset::Auto => PolicyPreset::Auto,
                Preset::ReadOnly => PolicyPreset::ReadOnly,
            }),
            edits: match c.edits {
                PresentEdits::Review => EditPolicy::Review,
                PresentEdits::AutoNewSlides => EditPolicy::AutoNewSlides,
                PresentEdits::Auto => EditPolicy::Auto,
                PresentEdits::Deny => EditPolicy::Deny,
            },
            code: gate(c.code),
            skills_scripts: gate(c.skills_scripts),
            code_mode: match c.code_mode {
                AllowDeny::Allow => Permission::Allow,
                AllowDeny::Deny => Permission::Deny,
            },
            export: gate(c.export),
            max_steps: c.max_steps,
            build_timeout_s: c.build_timeout_s,
        })
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

/// Provenance of a reference imported from Fibration Lab; all empty for `source = "file"`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct AnalystOrigin {
    pub repo: String,
    pub item: String,
    pub run_id: String,
}
