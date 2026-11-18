//! The deck document, schema version 1 (PROTOCOL 2).
//!
//! `deck/deck.json` is a [`Deck`]; each `deck/slides/<slide_id>.json` is a [`Slide`]. The
//! JSON Schemas in `schemas/` are generated from these types (see [`crate::schema`]).
//! Unknown fields are rejected throughout the document so that a misspelt key is a schema
//! error instead of silently ignored content. Lengths are EMU.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

pub use crate::cite::Cite;
pub use crate::ids::{ElementId, SlideId};

/// Placeholder key: a role (`"title"`, `"body"`, `"body-2"`, ...) or `"idx:<n>"` (PROTOCOL 2.2).
pub type PlaceholderKey = String;

/// A file under `assets/`, normally `"assets/<sha256>.<ext>"` (PROTOCOL 2.2). The schema
/// does not enforce the content-addressed form: the kernel's own test corpus uses plain
/// names such as `assets/wide.png`.
pub type AssetRef = String;

// ---- deck.json (2.1) ----

/// `deck/deck.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Deck {
    /// Always 1 for this protocol; see [`crate::SCHEMA_VERSION`].
    #[schemars(schema_with = "crate::schema::const_schema_version")]
    pub schema_version: u32,
    pub mode: DeckMode,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
    /// `"template/<file>.potx|pptx"`.
    pub template: String,
    /// Default from the template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slide_size: Option<Size>,
    /// Every slide id appears exactly once, in order.
    pub sections: Vec<Section>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub footer: Option<Footer>,
    /// Rendered where the template provides a footer placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub classification: Option<String>,
    /// The user's brief, for the agent's context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub brief: Option<String>,
    /// Custom document properties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub properties: Option<BTreeMap<String, String>>,
}

impl Deck {
    /// Slide ids in presentation order.
    pub fn slide_ids(&self) -> impl Iterator<Item = &SlideId> {
        self.sections.iter().flat_map(|s| s.slides.iter())
    }
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
pub enum DeckMode {
    #[default]
    Structured,
    Code,
}

impl std::fmt::Display for DeckMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DeckMode::Structured => "structured",
            DeckMode::Code => "code",
        })
    }
}

/// A width and height in EMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Size {
    #[schemars(range(min = 0))]
    pub cx: i64,
    #[schemars(range(min = 0))]
    pub cy: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub title: String,
    pub slides: Vec<SlideId>,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(deny_unknown_fields)]
pub struct Footer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slide_numbers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<bool>,
}

// ---- slide (2.2) ----

/// `deck/slides/<slide_id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Slide {
    pub id: SlideId,
    /// `"layout"` when absent.
    #[serde(default)]
    pub kind: SlideKind,
    /// Layout name from the template description.
    pub layout: String,
    /// Keyed by role (`"title"`, `"body"`, `"subtitle"`, `"picture"`, ...) or `"idx:<n>"`.
    pub placeholders: BTreeMap<PlaceholderKey, PlaceholderContent>,
    /// Only where the contract allows (rule `tpl.layout-only`); usually empty.
    pub elements: Vec<FreeElement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub notes: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub locked_elements: Option<Vec<ElementId>>,
    /// `kind = "generated"`: Python source run with `present_sdk` (PROTOCOL 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub generated: Option<Generated>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tags: Option<Vec<String>>,
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
pub enum SlideKind {
    #[default]
    Layout,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Generated {
    pub script: String,
}

/// What fills a layout placeholder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum PlaceholderContent {
    Text {
        id: ElementId,
        text: Text,
    },
    Image {
        id: ElementId,
        asset: AssetRef,
        alt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        fit: Option<ImageFit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        crop: Option<Crop>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        cite: Option<Vec<Cite>>,
        /// The analysis run this figure came from, when it was placed from a run of the
        /// project: a citation of the run itself, not of a reference.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        run_cite: Option<RunCite>,
    },
    Chart {
        id: ElementId,
        chart: Chart,
    },
    Table {
        id: ElementId,
        table: TableEl,
    },
    Diagram {
        id: ElementId,
        diagram: Diagram,
    },
    Equation {
        id: ElementId,
        /// The equation and where it came from: the function, its file and content hash,
        /// the provenance tier, and the numeric check.
        eq_cite: EqCite,
        /// The user has seen that the equation is unchecked (LaTeX-only) or disagrees with
        /// its code, and keeps it: the checker reports it as a warning instead of an error.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        acknowledged: Option<bool>,
    },
    Empty,
}

impl PlaceholderContent {
    /// The element id; `None` for `empty`.
    pub fn id(&self) -> Option<&ElementId> {
        match self {
            PlaceholderContent::Text { id, .. }
            | PlaceholderContent::Image { id, .. }
            | PlaceholderContent::Chart { id, .. }
            | PlaceholderContent::Table { id, .. }
            | PlaceholderContent::Diagram { id, .. }
            | PlaceholderContent::Equation { id, .. } => Some(id),
            PlaceholderContent::Empty => None,
        }
    }
}

/// A citation of an analysis run of the project (PROTOCOL 2.3): the run, the artifact it
/// produced, and what produced it. The image itself is copied into the deck's `assets/`
/// (content-addressed), so the deck stays self-contained; the run's data stays where it is.
/// When `tree_hash` no longer matches the project's working tree, the checker reports the
/// element as stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct RunCite {
    pub run_id: String,
    /// The artifact's repo-relative path as the run reported it.
    pub artifact: String,
    /// The script that was run, repo-relative.
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The identity of the project tree the run executed (`RunMeta.tree_hash`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tree_hash: Option<String>,
}

/// Where an equation on a slide came from (the equation record of the provenance contract,
/// PROTOCOL 11): the LaTeX the analysis kernel extracted for a function, its tier, the
/// numeric check of the code against it, and the source file's content hash when it was
/// placed. When the file's hash no longer matches the working tree, the checker reports the
/// element as stale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct EqCite {
    pub latex: String,
    pub tier: EqTier,
    pub check: EqCheckRecord,
    /// The function's qualified name in its module (`Model.predict`, `double_exp`).
    pub qualname: String,
    /// The source file, repo-relative.
    pub path: String,
    /// SHA-256 (hex) of the source file's content the equation was extracted from.
    pub source_hash: String,
}

/// The provenance tier of an equation (ARCHITECTURE 5.11): derived from the code, declared
/// in a `Math:` docstring section and numerically checked, or LaTeX for display only.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum EqTier {
    Derived,
    Declared,
    Latex,
}

/// The outcome of the numeric check of a function against its equation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum EqCheckState {
    Agree,
    Mismatch,
    Unavailable,
}

/// The numeric check as it was when the equation was placed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct EqCheckRecord {
    pub status: EqCheckState,
    #[serde(default)]
    pub trials: u64,
    /// Relative tolerance of the comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tol: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub counterexample: Option<EqCounterexample>,
    /// Why the check is unavailable, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

/// Inputs on which the code and the equation disagree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct EqCounterexample {
    #[ts(type = "Record<string, number>")]
    pub inputs: BTreeMap<String, f64>,
    pub expected: f64,
    pub actual: f64,
}

/// An element outside the layout's placeholders, with an explicit position and size in EMU.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FreeElement {
    pub id: ElementId,
    pub x: i64,
    pub y: i64,
    #[schemars(range(min = 0))]
    pub cx: i64,
    #[schemars(range(min = 0))]
    pub cy: i64,
    pub content: ElementContent,
}

/// Content of a [`FreeElement`]: any non-empty placeholder content, or a shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum ElementContent {
    Text {
        id: ElementId,
        text: Text,
    },
    Image {
        id: ElementId,
        asset: AssetRef,
        alt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        fit: Option<ImageFit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        crop: Option<Crop>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        cite: Option<Vec<Cite>>,
        /// The analysis run this figure came from, when it was placed from a run of the
        /// project: a citation of the run itself, not of a reference.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        run_cite: Option<RunCite>,
    },
    Chart {
        id: ElementId,
        chart: Chart,
    },
    Table {
        id: ElementId,
        table: TableEl,
    },
    Diagram {
        id: ElementId,
        diagram: Diagram,
    },
    Equation {
        id: ElementId,
        /// The equation and where it came from: the function, its file and content hash,
        /// the provenance tier, and the numeric check.
        eq_cite: EqCite,
        /// The user has seen that the equation is unchecked (LaTeX-only) or disagrees with
        /// its code, and keeps it: the checker reports it as a warning instead of an error.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        acknowledged: Option<bool>,
    },
    Shape {
        id: ElementId,
        shape: ShapeKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        fill: Option<ThemeColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        line: Option<ThemeColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        text: Option<Text>,
    },
}

impl ElementContent {
    pub fn id(&self) -> &ElementId {
        match self {
            ElementContent::Text { id, .. }
            | ElementContent::Image { id, .. }
            | ElementContent::Chart { id, .. }
            | ElementContent::Table { id, .. }
            | ElementContent::Diagram { id, .. }
            | ElementContent::Equation { id, .. }
            | ElementContent::Shape { id, .. } => id,
        }
    }
}

impl ElementContent {
    /// The same content as placeholder content; `None` for a shape.
    pub fn into_placeholder_content(self) -> Option<PlaceholderContent> {
        Some(match self {
            ElementContent::Text { id, text } => PlaceholderContent::Text { id, text },
            ElementContent::Image {
                id,
                asset,
                alt,
                fit,
                crop,
                cite,
                run_cite,
            } => PlaceholderContent::Image {
                id,
                asset,
                alt,
                fit,
                crop,
                cite,
                run_cite,
            },
            ElementContent::Chart { id, chart } => PlaceholderContent::Chart { id, chart },
            ElementContent::Table { id, table } => PlaceholderContent::Table { id, table },
            ElementContent::Diagram { id, diagram } => PlaceholderContent::Diagram { id, diagram },
            ElementContent::Equation {
                id,
                eq_cite,
                acknowledged,
            } => PlaceholderContent::Equation {
                id,
                eq_cite,
                acknowledged,
            },
            ElementContent::Shape { .. } => return None,
        })
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageFit {
    Fill,
    Fit,
    Crop,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum ShapeKind {
    Rect,
    RoundedRect,
    Ellipse,
    Line,
    Arrow,
}

/// Fractions `0..1` cut from each edge.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Crop {
    #[schemars(range(min = 0.0, max = 1.0))]
    pub l: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub t: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub r: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub b: f64,
}

// ---- text (2.2) ----

#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(deny_unknown_fields)]
pub struct Text {
    pub paragraphs: Vec<Paragraph>,
}

impl Text {
    /// One unformatted paragraph per line of `s`.
    pub fn plain(s: &str) -> Text {
        Text {
            paragraphs: s.lines().map(Paragraph::plain).collect(),
        }
    }

    /// The text without formatting, paragraphs joined by newlines.
    pub fn to_plain(&self) -> String {
        self.paragraphs
            .iter()
            .map(Paragraph::to_plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every citation on every run, in document order.
    pub fn cites(&self) -> impl Iterator<Item = &Cite> {
        self.paragraphs
            .iter()
            .flat_map(|p| p.runs.iter())
            .flat_map(|r| r.cite.iter().flatten())
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(deny_unknown_fields)]
pub struct Paragraph {
    /// Indent level 0 to 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 4))]
    #[ts(optional)]
    pub level: Option<u8>,
    pub runs: Vec<Run>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bullet: Option<Bullet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub align: Option<Align>,
}

impl Paragraph {
    pub fn plain(s: &str) -> Paragraph {
        Paragraph {
            runs: vec![Run::plain(s)],
            ..Default::default()
        }
    }

    pub fn to_plain(&self) -> String {
        self.runs.iter().map(|r| r.t.as_str()).collect()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Bullet {
    Auto,
    None,
    Number,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Auto,
    Left,
    Center,
    Right,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub t: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub b: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub i: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub u: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<ThemeColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link: Option<String>,
    /// An empty list marks the paragraph non-factual (PROTOCOL 7), which is different
    /// from an absent field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cite: Option<Vec<Cite>>,
}

impl Run {
    pub fn plain(s: &str) -> Run {
        Run {
            t: s.to_string(),
            ..Default::default()
        }
    }
}

/// A theme color slot; documents never carry hex colors.
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
pub enum ThemeColor {
    #[serde(rename = "dk1")]
    Dk1,
    #[serde(rename = "lt1")]
    Lt1,
    #[serde(rename = "dk2")]
    Dk2,
    #[serde(rename = "lt2")]
    Lt2,
    #[serde(rename = "accent1")]
    Accent1,
    #[serde(rename = "accent2")]
    Accent2,
    #[serde(rename = "accent3")]
    Accent3,
    #[serde(rename = "accent4")]
    Accent4,
    #[serde(rename = "accent5")]
    Accent5,
    #[serde(rename = "accent6")]
    Accent6,
    #[serde(rename = "hlink")]
    Hlink,
    #[serde(rename = "folHlink")]
    FolHlink,
}

impl ThemeColor {
    pub const ALL: [ThemeColor; 12] = [
        ThemeColor::Dk1,
        ThemeColor::Lt1,
        ThemeColor::Dk2,
        ThemeColor::Lt2,
        ThemeColor::Accent1,
        ThemeColor::Accent2,
        ThemeColor::Accent3,
        ThemeColor::Accent4,
        ThemeColor::Accent5,
        ThemeColor::Accent6,
        ThemeColor::Hlink,
        ThemeColor::FolHlink,
    ];
}

// ---- charts (2.4) ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Chart {
    pub r#type: ChartType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stacked: Option<Stacking>,
    pub source: ChartSource,
    /// Transform pipeline (PROTOCOL 2.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub steps: Option<Vec<TransformStep>>,
    pub series_from: SeriesFrom,
    /// Column name for categories (x).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub series: Option<Vec<ChartSeries>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub x_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub y_title: Option<String>,
    /// Excel format code, e.g. `"0.0%"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub number_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub labels: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub legend: Option<Legend>,
    /// Written by the core after `chart.compute`; the rendered values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub computed: Option<Table>,
    /// Reference version the values were computed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub computed_at_version: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum ChartType {
    Bar,
    Column,
    Line,
    Pie,
    Doughnut,
    Scatter,
    Area,
    Combo,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Stacking {
    None,
    Stacked,
    Percent,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum SeriesFrom {
    Columns,
    Rows,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Legend {
    None,
    Right,
    Bottom,
    Top,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ChartSeries {
    pub name: String,
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub r#type: Option<SeriesType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub axis: Option<SeriesAxis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<ThemeColor>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum SeriesType {
    Bar,
    Line,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum SeriesAxis {
    Primary,
    Secondary,
}

/// Where a chart's or table's data comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ChartSource {
    /// A range, named range, or extracted table.
    Ref { cite: Cite },
    /// Literal data; one cite per row or per value (rule `src.coverage`).
    Inline { table: Table, cites: Vec<Cite> },
}

/// A rectangular table of values (PROTOCOL 1.3 `Table`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<CellValue>>,
    /// The location the table was read from.
    pub loc: String,
}

/// One table cell: `string | number | null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(untagged)]
pub enum CellValue {
    Null,
    Number(#[ts(type = "number")] serde_json::Number),
    Text(String),
}

impl CellValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            CellValue::Number(n) => n.as_f64(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            CellValue::Text(s) => Some(s),
            _ => None,
        }
    }
}

impl From<&str> for CellValue {
    fn from(s: &str) -> Self {
        CellValue::Text(s.to_string())
    }
}

impl From<i64> for CellValue {
    fn from(n: i64) -> Self {
        CellValue::Number(n.into())
    }
}

impl From<f64> for CellValue {
    /// Non-finite values have no JSON form and become `Null`.
    fn from(n: f64) -> Self {
        serde_json::Number::from_f64(n)
            .map(CellValue::Number)
            .unwrap_or(CellValue::Null)
    }
}

// ---- tables and diagrams (2.5) ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TableEl {
    pub columns: u32,
    pub header_row: bool,
    pub banded_rows: bool,
    pub rows: Vec<TableRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<ChartSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub steps: Option<Vec<TransformStep>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TableCell {
    pub text: Text,
    /// Number of columns the cell spans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    #[ts(optional)]
    pub span: Option<u32>,
}

/// A native diagram primitive. Layout is computed by the kernel (PROTOCOL 5.3); the
/// document never carries coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "family", rename_all = "lowercase", deny_unknown_fields)]
pub enum Diagram {
    Process {
        variant: ProcessVariant,
        nodes: Vec<DNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        edges: Option<Vec<DEdge>>,
    },
    Timeline {
        variant: TimelineVariant,
        items: Vec<TimelineItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        lanes: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        axis: Option<TimelineAxis>,
    },
    Matrix {
        variant: MatrixVariant,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        axes: Option<MatrixAxes>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        rows: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        columns: Option<Vec<String>>,
        cells: Vec<MatrixCell>,
    },
    Hierarchy {
        variant: HierarchyVariant,
        nodes: Vec<HNode>,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum ProcessVariant {
    Chevrons,
    Steps,
    Flowchart,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum TimelineVariant {
    Milestones,
    Swimlanes,
    Gantt,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
pub enum MatrixVariant {
    #[serde(rename = "2x2")]
    TwoByTwo,
    #[serde(rename = "grid")]
    Grid,
    #[serde(rename = "kpi-tiles")]
    KpiTiles,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum HierarchyVariant {
    Org,
    Tree,
    DriverTree,
}

/// A diagram node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct DNode {
    pub id: String,
    pub label: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<DNodeKind>,
}

/// A hierarchy node: `DNode & { parent?, value? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct HNode {
    pub id: String,
    pub label: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<DNodeKind>,
    /// Id of the parent node; absent for a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub value: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum DNodeKind {
    Step,
    Decision,
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct DEdge {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
}

/// Dates are ISO 8601; `2026-03`, `2026-03-15`, and `2026-Q1` are accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TimelineItem {
    pub id: String,
    pub label: Text,
    pub start: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lane: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TimelineAxis {
    pub from: String,
    pub to: String,
    pub unit: TimeUnit,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum TimeUnit {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct MatrixAxes {
    pub x: String,
    pub y: String,
    pub x_low: String,
    pub x_high: String,
    pub y_low: String,
    pub y_high: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct MatrixCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub row: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub col: Option<u32>,
    /// Position in `0..1` for the 2x2 variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    #[ts(optional)]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    #[ts(optional)]
    pub y: Option<f64>,
    pub label: Text,
    /// Harvey ball score 0 to 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 4))]
    #[ts(optional)]
    pub score: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub value: Option<String>,
}

// ---- transforms (2.6) ----

/// One step of the closed transform set, applied in order to a [`Table`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransformStep {
    Select {
        columns: Vec<String>,
    },
    Rename {
        map: BTreeMap<String, String>,
    },
    Filter {
        column: String,
        cmp: Cmp,
        #[ts(type = "unknown")]
        value: Value,
    },
    Sort {
        by: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        descending: Option<bool>,
    },
    /// The remainder is summed into `other_label` when set.
    TopN {
        n: u32,
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        other_label: Option<String>,
    },
    GroupSum {
        by: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        columns: Option<Vec<String>>,
    },
    GroupMean {
        by: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        columns: Option<Vec<String>>,
    },
    GroupCount {
        by: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        columns: Option<Vec<String>>,
    },
    Pivot {
        index: String,
        columns: String,
        values: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agg: Option<Agg>,
    },
    Unpivot {
        id_columns: Vec<String>,
        var_name: String,
        value_name: String,
    },
    PctChange {
        column: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        periods: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        r#as: Option<String>,
    },
    ShareOfTotal {
        column: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        r#as: Option<String>,
    },
    Cumulative {
        column: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        r#as: Option<String>,
    },
    /// E.g. factor 0.001 for thousands.
    Scale {
        columns: Vec<String>,
        factor: f64,
    },
    Round {
        columns: Vec<String>,
        digits: i32,
    },
}

impl TransformStep {
    /// The `op` tag.
    pub fn op(&self) -> &'static str {
        match self {
            TransformStep::Select { .. } => "select",
            TransformStep::Rename { .. } => "rename",
            TransformStep::Filter { .. } => "filter",
            TransformStep::Sort { .. } => "sort",
            TransformStep::TopN { .. } => "top_n",
            TransformStep::GroupSum { .. } => "group_sum",
            TransformStep::GroupMean { .. } => "group_mean",
            TransformStep::GroupCount { .. } => "group_count",
            TransformStep::Pivot { .. } => "pivot",
            TransformStep::Unpivot { .. } => "unpivot",
            TransformStep::PctChange { .. } => "pct_change",
            TransformStep::ShareOfTotal { .. } => "share_of_total",
            TransformStep::Cumulative { .. } => "cumulative",
            TransformStep::Scale { .. } => "scale",
            TransformStep::Round { .. } => "round",
        }
    }
}

/// Comparison operator of the `filter` step.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
pub enum Cmp {
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "not_in")]
    NotIn,
    #[serde(rename = "contains")]
    Contains,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Agg {
    Sum,
    Mean,
    Count,
}
