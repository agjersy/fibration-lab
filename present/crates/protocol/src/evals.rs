//! Evaluation task definitions and scorecards (PROTOCOL 12).
//!
//! Same layout as Fibration Lab's `task.toml` (its PROTOCOL 8; base shape ported from
//! its `crates/protocol/src/evals.rs`), with Present's agent modes and check kinds.

use crate::deck::DeckMode;
use crate::ids::SlideId;
use serde::{Deserialize, Serialize};

/// Agent mode a task runs in (ARCHITECTURE 7.2).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Outline,
    #[default]
    Build,
    Polish,
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "outline" => Ok(Mode::Outline),
            "build" => Ok(Mode::Build),
            "polish" => Ok(Mode::Polish),
            other => Err(format!(
                "unknown mode `{other}` (expected outline|build|polish)"
            )),
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Mode::Outline => "outline",
            Mode::Build => "build",
            Mode::Polish => "polish",
        })
    }
}

/// `evals/tasks/<name>/task.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskFile {
    pub task: TaskSection,
    #[serde(default)]
    pub fixture: FixtureSection,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskSection {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
}

fn default_timeout() -> u64 {
    900
}

fn default_max_steps() -> u32 {
    40
}

/// The fixture is a project folder with `template/`, `references/`,
/// `.present/project.toml`, and an optional starting `deck/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureSection {
    pub dir: String,
}

impl Default for FixtureSection {
    fn default() -> Self {
        FixtureSection {
            dir: "fixture".to_string(),
        }
    }
}

/// A native element kind counted by `uses_native`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NativeKind {
    Chart,
    Table,
    Diagram,
}

/// Every check kind listed in PROTOCOL 12.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// The exported `.pptx` passes `out.opens-clean`.
    OpensClean,
    /// No errors; warnings <= `max_warnings` when set.
    CheckerClean {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_warnings: Option<u32>,
    },
    /// Visible slides within range.
    SlideCount {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<u32>,
    },
    /// Layout used at least `min` times (default 1).
    LayoutUsed {
        layout: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u32>,
    },
    /// At least `min` (default 1) native elements of that kind exist in the export.
    ///
    /// PROTOCOL 12 names this parameter `kind`, which is already the tag of the check
    /// table in `task.toml`; it is written `element` here.
    UsesNative {
        element: NativeKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u32>,
    },
    /// Factual paragraphs with citations / factual paragraphs >= `min` (0..1).
    CitationCoverage { min: f64 },
    /// Green or amber numbers / cited numbers >= `min` (0..1).
    NumbersVerified { min: f64 },
    /// A slide's text (or any slide's, without `slide`) matches.
    TextMatches {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slide: Option<SlideId>,
        regex: String,
    },
    /// Locked slides are byte-identical to the fixture.
    LockedUntouched,
    /// Final deck mode.
    Mode { mode: DeckMode },
    /// An export of that format exists and is non-empty.
    ExportExists { format: String },
    /// Only declared slides changed.
    EditsWithinScope,
}

impl Check {
    pub fn kind(&self) -> &'static str {
        match self {
            Check::OpensClean => "opens_clean",
            Check::CheckerClean { .. } => "checker_clean",
            Check::SlideCount { .. } => "slide_count",
            Check::LayoutUsed { .. } => "layout_used",
            Check::UsesNative { .. } => "uses_native",
            Check::CitationCoverage { .. } => "citation_coverage",
            Check::NumbersVerified { .. } => "numbers_verified",
            Check::TextMatches { .. } => "text_matches",
            Check::LockedUntouched => "locked_untouched",
            Check::Mode { .. } => "mode",
            Check::ExportExists { .. } => "export_exists",
            Check::EditsWithinScope => "edits_within_scope",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckResult {
    pub kind: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskResult {
    pub name: String,
    pub passed: bool,
    pub checks: Vec<CheckResult>,
    pub steps: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub wall_s: f64,
    pub unwanted_edits: u32,
    /// Tool calls that returned an error (feeds `tool_error_rate`).
    #[serde(default)]
    pub tool_errors: u32,
    #[serde(default)]
    pub tool_calls: u32,
}

/// Analyst's summary without its `contract_rate` and `eq_rate`, which have no Present
/// counterpart in PROTOCOL 12.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScorecardSummary {
    pub pass_rate: f64,
    pub unwanted_edit_rate: f64,
    pub tool_error_rate: f64,
}

/// Written to `evals/results/<model>-<timestamp>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Scorecard {
    pub model: String,
    pub tasks: Vec<TaskResult>,
    pub summary: ScorecardSummary,
}
