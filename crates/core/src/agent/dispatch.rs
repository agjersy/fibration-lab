//! Tool execution (PROTOCOL 6.3). Read tools run directly; edit tools are *prepared* into a
//! [`Change`] that the loop gates through policy and the approver before [`apply`] writes it.

use super::tools::Mode;
use crate::deck_store::Position;
use crate::util::{new_id, sha256_hex};
use crate::workspace::Workspace;
use base64::Engine as _;
use present_protocol::deck::{DeckMode, Slide};
use present_protocol::ids::SlideId;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The result of a tool call as the model sees it.
#[derive(Debug, Clone, Default)]
pub struct ToolOutcome {
    pub ok: bool,
    pub result: Value,
    /// Images to show the model alongside the result: `(media_type, base64)`.
    pub images: Vec<(String, String)>,
    /// Reference excerpts this call put into the model's context, for the audit log.
    pub excerpts: Vec<(String, u64)>,
    /// Slides the model has now looked at.
    pub viewed: Vec<String>,
}

impl ToolOutcome {
    pub fn ok(result: Value) -> ToolOutcome {
        ToolOutcome {
            ok: true,
            result,
            ..Default::default()
        }
    }

    pub fn err(message: impl Into<String>) -> ToolOutcome {
        ToolOutcome {
            ok: false,
            result: json!({"error": message.into()}),
            ..Default::default()
        }
    }
}

/// A row of a proposed storyline.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutlineRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide_id: Option<String>,
    pub title: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

/// A change to the deck awaiting a decision.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Outline {
        rows: Vec<OutlineRow>,
    },
    SlideCreate {
        slide: Box<Slide>,
        after: Option<SlideId>,
        section: Option<String>,
    },
    SlideUpdate {
        before: Box<Slide>,
        after: Box<Slide>,
        expected_hash: String,
    },
    SlideDelete {
        before: Box<Slide>,
    },
    SlideMove {
        id: SlideId,
        after: Option<SlideId>,
        section: Option<String>,
    },
    CodeEdit {
        path: String,
        original: Option<String>,
        proposed: String,
    },
    ModeSwitch {
        reason: String,
    },
}

impl Change {
    pub fn kind(&self) -> &'static str {
        match self {
            Change::Outline { .. } => "outline",
            Change::SlideCreate { .. } => "slide-create",
            Change::SlideUpdate { .. } => "slide-update",
            Change::SlideDelete { .. } => "slide-delete",
            Change::SlideMove { .. } => "slide-move",
            Change::CodeEdit { .. } => "code-edit",
            Change::ModeSwitch { .. } => "mode-switch",
        }
    }

    /// The existing slides (or files) this change touches.
    pub fn targets(&self) -> Vec<String> {
        match self {
            Change::SlideUpdate { before, .. } | Change::SlideDelete { before } => {
                vec![before.id.to_string()]
            }
            Change::SlideMove { id, .. } => vec![id.to_string()],
            Change::CodeEdit { path, .. } => vec![path.clone()],
            _ => Vec::new(),
        }
    }

    pub fn creates_new(&self) -> bool {
        matches!(self, Change::SlideCreate { .. })
            || matches!(self, Change::CodeEdit { original: None, .. })
    }

    pub fn slide_ids(&self) -> Vec<String> {
        match self {
            Change::SlideCreate { slide, .. } => vec![slide.id.to_string()],
            Change::SlideUpdate { after, .. } => vec![after.id.to_string()],
            Change::SlideDelete { before } => vec![before.id.to_string()],
            Change::SlideMove { id, .. } => vec![id.to_string()],
            _ => Vec::new(),
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Change::Outline { rows } => format!("Storyline of {} slides", rows.len()),
            Change::SlideCreate { slide, .. } => {
                format!("New slide {}: {}", slide.id, title_of(slide))
            }
            Change::SlideUpdate { after, .. } => {
                format!("Change slide {}: {}", after.id, title_of(after))
            }
            Change::SlideDelete { before } => {
                format!("Delete slide {}: {}", before.id, title_of(before))
            }
            Change::SlideMove { id, after, section } => match (after, section) {
                (Some(a), _) => format!("Move slide {id} after {a}"),
                (None, Some(s)) => format!("Move slide {id} to section {s:?}"),
                _ => format!("Move slide {id} to the front"),
            },
            Change::CodeEdit { path, original, .. } => format!(
                "{} {path}",
                if original.is_some() { "Edit" } else { "Create" }
            ),
            Change::ModeSwitch { reason } => format!("Switch to code generation mode: {reason}"),
        }
    }

    /// A textual before/after for review cards and the transcript.
    pub fn diff(&self) -> String {
        match self {
            Change::Outline { rows } => rows
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {} — {}", i + 1, r.title, r.message))
                .collect::<Vec<_>>()
                .join("\n"),
            Change::SlideCreate { slide, .. } => line_diff("", &slide_text(slide)),
            Change::SlideUpdate { before, after, .. } => {
                line_diff(&slide_text(before), &slide_text(after))
            }
            Change::SlideDelete { before } => line_diff(&slide_text(before), ""),
            Change::CodeEdit {
                original, proposed, ..
            } => line_diff(original.as_deref().unwrap_or(""), proposed),
            _ => String::new(),
        }
    }
}

/// What `prepare` produced.
pub enum Prepared {
    Done(ToolOutcome),
    Change(Change),
    /// Refused before policy: a locked slide, or a mode restriction.
    Denied {
        reason: &'static str,
        message: String,
    },
}

/// Per-conversation state the tools read and update.
#[derive(Default)]
pub struct ToolState {
    pub template: Option<Value>,
    pub pinned: Vec<String>,
    /// Latest checker counts per slide: `(errors, warnings)`.
    pub status: BTreeMap<String, (usize, usize)>,
    pub render_unavailable: bool,
    pub skills: Option<wb_skills::SkillSet>,
    pub disabled_skills: BTreeSet<String>,
}

pub const AGENT_PPTX: &str = ".present/cache/agent/deck.pptx";

// ---- slide helpers ---------------------------------------------------------------------------

fn text_of(value: &Value) -> String {
    let mut out = Vec::new();
    for para in value["paragraphs"].as_array().into_iter().flatten() {
        let line: String = para["runs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["t"].as_str())
            .collect();
        let level = para["level"].as_u64().unwrap_or(0) as usize;
        out.push(format!("{}{}", "  ".repeat(level), line));
    }
    out.join("\n")
}

pub fn title_of(slide: &Slide) -> String {
    let v = serde_json::to_value(slide).unwrap_or(Value::Null);
    text_of(&v["placeholders"]["title"]["text"]).replace('\n', " ")
}

/// A readable rendering of a slide's content, for diffs and search.
pub fn slide_text(slide: &Slide) -> String {
    let v = serde_json::to_value(slide).unwrap_or(Value::Null);
    let mut out = vec![format!("layout: {}", slide.layout)];
    let describe = |key: &str, item: &Value, out: &mut Vec<String>| match item["type"].as_str() {
        Some("text") => out.push(format!("[{key}]\n{}", text_of(&item["text"]))),
        Some("chart") => out.push(format!(
            "[{key}] chart {} {}",
            item["chart"]["type"].as_str().unwrap_or(""),
            item["chart"]["title"].as_str().unwrap_or("")
        )),
        Some("table") => {
            let rows: Vec<String> = item["table"]["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    r["cells"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| text_of(&c["text"]))
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect();
            out.push(format!("[{key}] table\n{}", rows.join("\n")));
        }
        Some("image") => out.push(format!(
            "[{key}] image {} ({})",
            item["asset"].as_str().unwrap_or(""),
            item["alt"].as_str().unwrap_or("")
        )),
        Some("diagram") => out.push(format!(
            "[{key}] diagram {} {}",
            item["diagram"]["family"].as_str().unwrap_or(""),
            item["diagram"]["variant"].as_str().unwrap_or("")
        )),
        Some("shape") => out.push(format!("[{key}] shape {}", text_of(&item["text"]))),
        _ => {}
    };
    if let Some(map) = v["placeholders"].as_object() {
        if let Some(title) = map.get("title") {
            describe("title", title, &mut out);
        }
        for (key, item) in map.iter().filter(|(k, _)| k.as_str() != "title") {
            describe(key, item, &mut out);
        }
    }
    for (i, el) in v["elements"].as_array().into_iter().flatten().enumerate() {
        describe(&format!("element {i}"), &el["content"], &mut out);
    }
    if v["notes"].is_object() {
        out.push(format!("[notes]\n{}", text_of(&v["notes"])));
    }
    out.join("\n")
}

/// Line-based diff: removed lines prefixed `-`, added `+`, with unchanged lines kept as context.
pub fn line_diff(before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    // Longest common subsequence table; slides and scripts are small.
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(format!("  {}", a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| format!("- {l}")));
    out.extend(b[j..].iter().map(|l| format!("+ {l}")));
    out.join("\n")
}

fn collect_cites(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == "cite" || k == "cites" || k == "sources" {
                    match v {
                        Value::String(s) => {
                            out.insert(s.clone());
                        }
                        Value::Array(items) => {
                            out.extend(items.iter().filter_map(|x| x.as_str().map(str::to_string)))
                        }
                        _ => {}
                    }
                }
                collect_cites(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_cites(v, out)),
        _ => {}
    }
}

fn element_ids(value: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for item in value["placeholders"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        if let Some(id) = item["id"].as_str() {
            out.insert(id.to_string());
        }
    }
    for el in value["elements"].as_array().into_iter().flatten() {
        if let Some(id) = el["id"].as_str() {
            out.insert(id.to_string());
        }
    }
    out
}

/// Fill in what the model may omit: `kind`, `elements`, element ids.
fn normalize_slide(value: &mut Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    map.entry("kind").or_insert(json!("layout"));
    map.entry("elements").or_insert(json!([]));
    map.entry("placeholders").or_insert(json!({}));
    if let Some(phs) = map.get_mut("placeholders").and_then(|p| p.as_object_mut()) {
        for item in phs.values_mut() {
            if let Some(obj) = item.as_object_mut() {
                if obj.get("type").and_then(|t| t.as_str()) != Some("empty")
                    && !obj.contains_key("id")
                {
                    obj.insert("id".into(), json!(new_id("e", 6)));
                }
            }
        }
    }
    if let Some(els) = map.get_mut("elements").and_then(|e| e.as_array_mut()) {
        for el in els {
            if let Some(obj) = el.as_object_mut() {
                let id = obj
                    .get("id")
                    .and_then(|i| i.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| new_id("e", 6));
                obj.insert("id".into(), json!(id));
                if let Some(content) = obj.get_mut("content").and_then(|c| c.as_object_mut()) {
                    content.entry("id").or_insert(json!(id));
                }
            }
        }
    }
}

fn parse_slide(mut value: Value) -> Result<Slide, String> {
    normalize_slide(&mut value);
    serde_json::from_value::<Slide>(value)
        .map_err(|e| format!("the slide document is not valid: {e}"))
}

/// Compute `computed` for every chart whose data comes from a reference or has transform steps.
async fn compute_charts(ws: &Workspace, value: &mut Value) -> Result<(), String> {
    let mut charts: Vec<&mut Value> = Vec::new();
    if let Some(map) = value["placeholders"].as_object_mut() {
        for item in map.values_mut() {
            if item["type"] == "chart" {
                charts.push(&mut item["chart"]);
            }
        }
    }
    // Free elements are collected in a second pass to satisfy the borrow of `value`.
    for chart in charts {
        compute_chart(ws, chart).await?;
    }
    if let Some(els) = value["elements"].as_array_mut() {
        for el in els {
            if el["content"]["type"] == "chart" {
                compute_chart(ws, &mut el["content"]["chart"]).await?;
            }
        }
    }
    Ok(())
}

async fn compute_chart(ws: &Workspace, chart: &mut Value) -> Result<(), String> {
    let from_ref = chart["source"]["kind"] == "ref";
    let has_steps = chart["steps"].as_array().is_some_and(|s| !s.is_empty());
    if !from_ref && !has_steps {
        return Ok(());
    }
    let out = ws
        .kernel
        .call_value("chart.compute", json!({"source": chart["source"], "steps": chart.get("steps").cloned().unwrap_or(json!([]))}))
        .await
        .map_err(|e| format!("the chart's data could not be computed: {e}"))?;
    chart["computed"] = out["table"].clone();
    if let Some(cite) = chart["source"]["cite"].as_str() {
        if let Some(version) = cite
            .split('@')
            .nth(1)
            .and_then(|rest| rest.split('#').next())
        {
            chart["computed_at_version"] = json!(version);
        }
    }
    Ok(())
}

async fn template(ws: &Workspace, state: &mut ToolState) -> Result<Value, String> {
    if let Some(t) = &state.template {
        return Ok(t.clone());
    }
    let deck = ws.deck().read_deck().map_err(|e| e.to_string())?;
    let desc = ws
        .kernel
        .call_value("template.inspect", json!({"path": deck.template}))
        .await
        .map_err(|e| e.to_string())?;
    state.template = Some(desc.clone());
    Ok(desc)
}

fn layouts(desc: &Value) -> impl Iterator<Item = &Value> {
    desc["masters"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["layouts"].as_array().into_iter().flatten())
}

const FURNITURE: &[&str] = &["dt", "ftr", "sldNum"];

/// The compact layout catalog given to the model.
pub fn layout_catalog(desc: &Value) -> Value {
    let rows: Vec<Value> = layouts(desc)
        .filter(|l| l["usable"].as_bool().unwrap_or(true))
        .map(|l| {
            let phs: Vec<Value> = l["placeholders"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| !FURNITURE.contains(&p["type"].as_str().unwrap_or("")))
                .map(|p| {
                    let size = p["levels"][0]["size_pt"].as_f64().unwrap_or(18.0).max(1.0);
                    let per_line = (p["cx"].as_f64().unwrap_or(0.0) / 12700.0 / (size * 0.5)).floor().max(1.0);
                    let lines = (p["cy"].as_f64().unwrap_or(0.0) / 12700.0 / (size * 1.2)).floor().max(1.0);
                    json!({"key": p["key"], "accepts": p["accepts"], "holds": format!("about {} lines of {} characters", lines as u32, per_line as u32)})
                })
                .collect();
            let mut row = json!({"layout": l["name"], "placeholders": phs});
            if let Some(d) = l["description"].as_str() {
                row["use_for"] = json!(d);
            }
            row
        })
        .collect();
    json!({"layouts": rows, "theme_colors": ["dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6"]})
}

fn check_layout(desc: &Value, slide: &Slide) -> Result<(), String> {
    let Some(layout) = layouts(desc).find(|l| l["name"] == slide.layout.as_str()) else {
        let names: Vec<&str> = layouts(desc)
            .filter(|l| l["usable"].as_bool().unwrap_or(true))
            .filter_map(|l| l["name"].as_str())
            .collect();
        return Err(format!(
            "the template has no layout {:?}; choose one of: {}",
            slide.layout,
            names.join(", ")
        ));
    };
    let v = serde_json::to_value(slide).map_err(|e| e.to_string())?;
    for (key, item) in v["placeholders"].as_object().into_iter().flatten() {
        let found = layout["placeholders"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["key"] == key.as_str() || format!("idx:{}", p["idx"]) == *key);
        let Some(ph) = found else {
            let keys: Vec<&str> = layout["placeholders"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| !FURNITURE.contains(&p["type"].as_str().unwrap_or("")))
                .filter_map(|p| p["key"].as_str())
                .collect();
            return Err(format!(
                "layout {:?} has no placeholder {key:?}; its placeholders are: {}",
                slide.layout,
                keys.join(", ")
            ));
        };
        let kind = item["type"].as_str().unwrap_or("empty");
        if kind != "empty"
            && !ph["accepts"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x == kind))
        {
            return Err(format!(
                "placeholder {key:?} of layout {:?} does not accept {kind}; it accepts {}",
                slide.layout, ph["accepts"]
            ));
        }
    }
    Ok(())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` is required"))
}

fn slide_id_arg(args: &Value, key: &str) -> Result<SlideId, String> {
    SlideId::new(str_arg(args, key)?.to_string()).map_err(|_| format!("`{key}` is not a slide id"))
}

fn opt_slide_id(args: &Value, key: &str) -> Result<Option<SlideId>, String> {
    match args.get(key).and_then(|v| v.as_str()) {
        Some(s) => SlideId::new(s.to_string())
            .map(Some)
            .map_err(|_| format!("`{key}` is not a slide id")),
        None => Ok(None),
    }
}

fn safe_deck_path(path: &str) -> Result<String, String> {
    let p = path.replace('\\', "/");
    if p.starts_with('/')
        || p.split('/').any(|c| c == ".." || c.is_empty())
        || !p.starts_with("deck/")
        || !p.ends_with(".py")
    {
        return Err("only Python files under deck/ can be read or edited here".into());
    }
    Ok(p)
}

/// Build the whole deck into the agent cache; returns the kernel's result.
pub async fn build_agent_deck(ws: &Workspace) -> Result<Value, String> {
    ws.build(AGENT_PPTX, "", None)
        .await
        .map_err(|e| build_error(&e))
}

fn build_error(e: &crate::kernel::KernelError) -> String {
    let mut text = e.to_string();
    for key in ["stderr", "stdout"] {
        if let Some(s) = e
            .data(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            let tail: String = s
                .chars()
                .rev()
                .take(1500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            text.push_str(&format!("\n{key}:\n{tail}"));
        }
    }
    text
}

/// Run the checker on (some slides of) the freshly built agent deck and record the status.
pub async fn check_slides(
    ws: &Workspace,
    state: &mut ToolState,
    slides: Option<&[String]>,
) -> Result<Vec<Value>, String> {
    build_agent_deck(ws).await?;
    let diags = ws
        .check(AGENT_PPTX, slides, None, false)
        .await
        .map_err(|e| e.to_string())?;
    let scope: Vec<String> = match slides {
        Some(s) => s.to_vec(),
        None => ws
            .deck()
            .read_deck()
            .map(|d| d.slide_ids().map(|s| s.to_string()).collect())
            .unwrap_or_default(),
    };
    for id in scope {
        let count = |sev: &str| {
            diags
                .iter()
                .filter(|d| d["slide_id"] == id.as_str() && d["severity"] == sev)
                .count()
        };
        state
            .status
            .insert(id.clone(), (count("error"), count("warn")));
    }
    Ok(diags)
}

fn compact_diags(diags: &[Value]) -> Value {
    let rows: Vec<Value> = diags
        .iter()
        .filter(|d| d["severity"] != "info")
        .take(40)
        .map(|d| {
            let mut row =
                json!({"rule": d["rule"], "severity": d["severity"], "message": d["message"]});
            for key in ["slide_id", "element_id", "fix_hint", "pointer"] {
                if !d[key].is_null() {
                    row[key] = d[key].clone();
                }
            }
            row
        })
        .collect();
    json!(rows)
}

// ---- prepare ---------------------------------------------------------------------------------

pub async fn prepare(
    ws: &Workspace,
    state: &mut ToolState,
    mode: Mode,
    name: &str,
    args: &Value,
) -> Prepared {
    match prepare_inner(ws, state, mode, name, args).await {
        Ok(p) => p,
        Err(message) => Prepared::Done(ToolOutcome::err(message)),
    }
}

async fn prepare_inner(
    ws: &Workspace,
    state: &mut ToolState,
    mode: Mode,
    name: &str,
    args: &Value,
) -> Result<Prepared, String> {
    let store = ws.deck();
    let done = |v: Value| Ok(Prepared::Done(ToolOutcome::ok(v)));
    match name {
        "deck.outline" => {
            let (deck, slides) = store.read_all().map_err(|e| e.to_string())?;
            let mut rows = Vec::new();
            let mut it = slides.iter();
            for section in &deck.sections {
                for _ in &section.slides {
                    let Some(slide) = it.next() else { break };
                    let mut row =
                        json!({"id": slide.id, "layout": slide.layout, "title": title_of(slide)});
                    if !section.title.is_empty() {
                        row["section"] = json!(section.title);
                    }
                    if slide.locked == Some(true) {
                        row["locked"] = json!(true);
                    }
                    if slide.hidden == Some(true) {
                        row["hidden"] = json!(true);
                    }
                    if let Some((e, w)) = state.status.get(slide.id.as_str()) {
                        row["checker"] = json!(format!("{e} errors, {w} warnings"));
                    }
                    rows.push(row);
                }
            }
            let mut out = json!({"title": deck.title, "mode": deck.mode, "slides": rows});
            if let Some(brief) = &deck.brief {
                out["brief"] = json!(brief);
            }
            let outline = ws.project.root.join(".present").join("outline.json");
            if let Ok(text) = std::fs::read_to_string(outline) {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    out["approved_outline"] = v;
                }
            }
            done(out)
        }
        "deck.read_slide" => {
            let (slide, hash) = store
                .read_slide(&slide_id_arg(args, "slide_id")?)
                .map_err(|e| e.to_string())?;
            done(json!({"slide": slide, "hash": hash}))
        }
        "deck.search" => {
            let query = str_arg(args, "query")?.to_lowercase();
            let (_, slides) = store.read_all().map_err(|e| e.to_string())?;
            let hits: Vec<Value> = slides
                .iter()
                .filter_map(|s| {
                    let text = slide_text(s);
                    let line = text.lines().find(|l| l.to_lowercase().contains(&query))?;
                    Some(json!({"slide_id": s.id, "title": title_of(s), "match": line.trim()}))
                })
                .collect();
            done(json!({"hits": hits}))
        }
        "template.layouts" => done(layout_catalog(&template(ws, state).await?)),
        "template.layout_detail" => {
            let desc = template(ws, state).await?;
            let wanted = str_arg(args, "layout")?;
            let layout = layouts(&desc)
                .find(|l| l["name"] == wanted)
                .cloned()
                .ok_or_else(|| format!("no layout {wanted:?}"))?;
            done(layout)
        }
        "refs.list" => {
            let rows = ws.reference_rows().map_err(|e| e.to_string())?;
            let list: Vec<Value> = rows
                .iter()
                .map(|r| {
                    let outline: Value = serde_json::from_str(&r.outline_json).unwrap_or(json!([]));
                    let top: Vec<Value> = outline.as_array().into_iter().flatten().take(30).map(|n| json!({"title": n["title"], "loc": n["loc"]})).collect();
                    json!({"ref_id": r.id, "version": r.version, "kind": r.kind, "title": r.title, "cite_prefix": format!("ref:{}@{}#", r.id, r.version), "outline": top})
                })
                .collect();
            done(json!({"references": list}))
        }
        "refs.search" => {
            let ids: Option<Vec<String>> =
                args.get("ref_ids").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                });
            let k = args
                .get("k")
                .and_then(|v| v.as_u64())
                .unwrap_or(8)
                .clamp(1, 20) as usize;
            let hits = ws
                .search(str_arg(args, "query")?, ids.as_deref(), k)
                .await
                .map_err(|e| e.to_string())?;
            let mut outcome = ToolOutcome::ok(
                json!({"hits": hits.iter().map(|h| json!({"cite": h.cite(), "title": h.title, "heading_path": h.heading_path, "snippet": h.snippet})).collect::<Vec<_>>()}),
            );
            outcome.excerpts = hits
                .iter()
                .map(|h| (h.cite(), h.snippet.chars().count() as u64))
                .collect();
            Ok(Prepared::Done(outcome))
        }
        "refs.read" | "refs.table" => {
            let cite = str_arg(args, "cite")?;
            let mut params = json!({"cite": cite});
            if let Some(n) = args.get("max_chars") {
                params["max_chars"] = n.clone();
            }
            let out = ws
                .kernel
                .call_value(name, params)
                .await
                .map_err(|e| e.to_string())?;
            let chars = out.to_string().chars().count() as u64;
            let mut outcome = ToolOutcome::ok(out);
            outcome.excerpts.push((cite.to_string(), chars));
            Ok(Prepared::Done(outcome))
        }
        "refs.image" => {
            let ref_id = str_arg(args, "ref_id")?;
            let page = args
                .get("page")
                .and_then(|v| v.as_u64())
                .ok_or("`page` is required")?;
            let out = ws
                .kernel
                .call_value(
                    "refs.page_image",
                    json!({"ref_id": ref_id, "page": page, "dpi": 110}),
                )
                .await
                .map_err(|e| e.to_string())?;
            let rel = out["png"].as_str().ok_or("the kernel returned no image")?;
            let bytes = std::fs::read(ws.project.root.join(rel)).map_err(|e| e.to_string())?;
            let mut outcome = ToolOutcome::ok(json!({"page": page, "ref_id": ref_id}));
            outcome.images.push((
                "image/png".into(),
                base64::engine::general_purpose::STANDARD.encode(bytes),
            ));
            Ok(Prepared::Done(outcome))
        }
        "slide.render" => {
            let id = slide_id_arg(args, "slide_id")?;
            let built = build_agent_deck(ws).await?;
            let deck = store.read_deck().map_err(|e| e.to_string())?;
            // Hidden slides are not rendered by the engine, so the page index counts visible ones.
            let order: Vec<String> = built["element_map"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["slide_id"].as_str().map(str::to_string))
                .fold(Vec::new(), |mut acc, s| {
                    if !acc.contains(&s) {
                        acc.push(s);
                    }
                    acc
                });
            let visible: Vec<String> = if deck.mode == DeckMode::Structured {
                deck.slide_ids()
                    .filter(|s| {
                        store
                            .read_slide(s)
                            .map(|(sl, _)| sl.hidden != Some(true))
                            .unwrap_or(true)
                    })
                    .map(|s| s.to_string())
                    .collect()
            } else {
                order
            };
            let Some(index) = visible.iter().position(|s| s == id.as_str()) else {
                return Err(format!("slide {id} is hidden or not in the deck"));
            };
            match ws.kernel.call_value("render.fidelity", json!({"pptx": AGENT_PPTX, "format": "png", "slides": [index], "dpi": 96, "out_dir": ".present/cache/agent/png"})).await {
                Ok(out) => {
                    let rel = out["files"][0].as_str().ok_or("the engine produced no image")?;
                    let bytes = std::fs::read(ws.project.root.join(rel)).map_err(|e| e.to_string())?;
                    let mut outcome = ToolOutcome::ok(json!({"slide_id": id, "engine": out["engine"], "note": "the image follows"}));
                    outcome.images.push(("image/png".into(), base64::engine::general_purpose::STANDARD.encode(bytes)));
                    outcome.viewed.push(id.to_string());
                    Ok(Prepared::Done(outcome))
                }
                Err(e) if e.code() == Some(2008) => {
                    state.render_unavailable = true;
                    Err("no rendering engine is installed (PowerPoint or LibreOffice), so slides cannot be looked at; rely on check_run".into())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "check.run" => {
            let ids: Option<Vec<String>> =
                args.get("slide_ids").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                });
            let diags = check_slides(ws, state, ids.as_deref()).await?;
            let errors = diags.iter().filter(|d| d["severity"] == "error").count();
            let warnings = diags.iter().filter(|d| d["severity"] == "warn").count();
            done(json!({"errors": errors, "warnings": warnings, "findings": compact_diags(&diags)}))
        }
        "chart.preview" => {
            let out = ws.kernel.call_value("chart.compute", json!({"source": args["source"], "steps": args.get("steps").cloned().unwrap_or(json!([]))})).await.map_err(|e| e.to_string())?;
            done(json!({"table": out["table"]}))
        }
        "skill.load" => {
            let skills = state.skills.as_ref().ok_or("no skills are installed")?;
            let name = str_arg(args, "name")?;
            if state.disabled_skills.contains(name) {
                return Err(format!("skill {name:?} is disabled for this project"));
            }
            done(
                json!({"name": name, "instructions": skills.load(name).map_err(|e| e.to_string())?}),
            )
        }
        "skill.read_resource" => {
            let skills = state.skills.as_ref().ok_or("no skills are installed")?;
            match skills
                .read_resource(str_arg(args, "name")?, str_arg(args, "path")?)
                .map_err(|e| e.to_string())?
            {
                wb_skills::Resource::Text(text) => done(json!({"text": text})),
                wb_skills::Resource::Image { media_type, bytes } => {
                    let mut outcome = ToolOutcome::ok(json!({"note": "the image follows"}));
                    outcome.images.push((
                        media_type,
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ));
                    Ok(Prepared::Done(outcome))
                }
                wb_skills::Resource::TooLarge { size, .. } => {
                    Err(format!("the resource is too large to read ({size} bytes)"))
                }
            }
        }
        "facts.pin" => {
            let fact = str_arg(args, "fact")?.trim().to_string();
            if !fact.is_empty() && !state.pinned.contains(&fact) {
                state.pinned.push(fact);
            }
            done(json!({"pinned": state.pinned.len()}))
        }
        "outline.propose" => {
            let rows: Vec<OutlineRow> = serde_json::from_value(args["rows"].clone())
                .map_err(|e| format!("`rows` is not valid: {e}"))?;
            if rows.is_empty() {
                return Err("the outline has no rows".into());
            }
            Ok(Prepared::Change(Change::Outline { rows }))
        }
        "slide.propose_create" => {
            let mut value = args.get("slide").cloned().ok_or("`slide` is required")?;
            if value.get("id").and_then(|i| i.as_str()).is_none() {
                value["id"] = json!(store.new_slide_id());
            }
            normalize_slide(&mut value);
            compute_charts(ws, &mut value).await?;
            let slide = parse_slide(value)?;
            if store.slide_path(&slide.id).exists() {
                return Err(format!(
                    "slide {} already exists; use slide_propose_update",
                    slide.id
                ));
            }
            check_layout(&template(ws, state).await?, &slide)?;
            Ok(Prepared::Change(Change::SlideCreate {
                slide: Box::new(slide),
                after: opt_slide_id(args, "after")?,
                section: args
                    .get("section")
                    .and_then(|s| s.as_str())
                    .map(str::to_string),
            }))
        }
        "slide.propose_update" => {
            let id = slide_id_arg(args, "slide_id")?;
            let (before, hash) = store.read_slide(&id).map_err(|e| e.to_string())?;
            if before.locked == Some(true) {
                return Ok(Prepared::Denied {
                    reason: "locked",
                    message: format!("slide {id} is locked by the user and cannot be changed"),
                });
            }
            let before_value = serde_json::to_value(&before).map_err(|e| e.to_string())?;
            let mut value = if let Some(patch) = args.get("patch").filter(|p| p.is_array()) {
                let ops: json_patch::Patch = serde_json::from_value(patch.clone())
                    .map_err(|e| format!("`patch` is not a valid JSON Patch: {e}"))?;
                let mut doc = before_value.clone();
                json_patch::patch(&mut doc, &ops).map_err(|e| {
                    format!(
                        "the patch does not apply to the slide as it is now (read it again): {e}"
                    )
                })?;
                doc
            } else if let Some(slide) = args.get("slide").filter(|s| s.is_object()) {
                slide.clone()
            } else {
                return Err("give either `slide` or `patch`".into());
            };
            value["id"] = json!(id);
            if before.locked_elements.is_some() && value.get("locked_elements").is_none() {
                value["locked_elements"] = before_value["locked_elements"].clone();
            }
            normalize_slide(&mut value);
            compute_charts(ws, &mut value).await?;
            for locked in before.locked_elements.iter().flatten() {
                let find = |v: &Value| -> Option<Value> {
                    v["placeholders"]
                        .as_object()
                        .into_iter()
                        .flat_map(|m| m.values())
                        .chain(v["elements"].as_array().into_iter().flatten())
                        .find(|i| i["id"] == locked.as_str())
                        .cloned()
                };
                if find(&before_value) != find(&value) {
                    return Ok(Prepared::Denied {
                        reason: "locked",
                        message: format!(
                            "element {locked} is locked by the user and cannot be changed"
                        ),
                    });
                }
            }
            if mode == Mode::Polish {
                let (mut old_cites, mut new_cites) = (BTreeSet::new(), BTreeSet::new());
                collect_cites(&before_value, &mut old_cites);
                collect_cites(&value, &mut new_cites);
                if let Some(added) = new_cites.difference(&old_cites).next() {
                    return Ok(Prepared::Denied { reason: "mode", message: format!("polish mode cannot add claims: {added} is a new citation. Switch to build mode to add content") });
                }
                if let Some(added) = element_ids(&value)
                    .difference(&element_ids(&before_value))
                    .next()
                {
                    return Ok(Prepared::Denied { reason: "mode", message: format!("polish mode cannot add content: element {added} is new. Switch to build mode to add content") });
                }
            }
            let after = parse_slide(value)?;
            check_layout(&template(ws, state).await?, &after)?;
            if after == before {
                return Err("the proposed slide is identical to the current one".into());
            }
            Ok(Prepared::Change(Change::SlideUpdate {
                before: Box::new(before),
                after: Box::new(after),
                expected_hash: hash,
            }))
        }
        "slide.propose_delete" => {
            let (before, _) = store
                .read_slide(&slide_id_arg(args, "slide_id")?)
                .map_err(|e| e.to_string())?;
            if before.locked == Some(true) {
                return Ok(Prepared::Denied {
                    reason: "locked",
                    message: format!(
                        "slide {} is locked by the user and cannot be deleted",
                        before.id
                    ),
                });
            }
            Ok(Prepared::Change(Change::SlideDelete {
                before: Box::new(before),
            }))
        }
        "slide.propose_move" => {
            let id = slide_id_arg(args, "slide_id")?;
            store.read_slide(&id).map_err(|e| e.to_string())?;
            Ok(Prepared::Change(Change::SlideMove {
                id,
                after: opt_slide_id(args, "after")?,
                section: args
                    .get("section")
                    .and_then(|s| s.as_str())
                    .map(str::to_string),
            }))
        }
        "asset.import" => {
            let cite = str_arg(args, "cite")?;
            let ref_id = cite
                .strip_prefix("ref:")
                .and_then(|r| r.split('@').next())
                .ok_or("`cite` is not a citation")?;
            let manifest =
                crate::refs::load_manifest(&ws.project.root).map_err(|e| e.to_string())?;
            let entry = manifest
                .refs
                .iter()
                .find(|r| r.id.as_str() == ref_id)
                .ok_or_else(|| format!("no reference {ref_id}"))?;
            let kind = serde_json::to_value(entry.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let bytes = match kind.as_str() {
                "image" | "analyst-figure" => {
                    std::fs::read(ws.project.root.join(&entry.file)).map_err(|e| e.to_string())?
                }
                "pdf" => {
                    let page: u64 = cite
                        .split('#')
                        .nth(1)
                        .and_then(|l| l.strip_prefix('p'))
                        .and_then(|l| l.split([':', '/']).next())
                        .and_then(|n| n.parse().ok())
                        .ok_or("a PDF citation needs a page (…#p3)")?;
                    let out = ws
                        .kernel
                        .call_value(
                            "refs.page_image",
                            json!({"ref_id": ref_id, "page": page, "dpi": 200}),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    std::fs::read(
                        ws.project
                            .root
                            .join(out["png"].as_str().unwrap_or_default()),
                    )
                    .map_err(|e| e.to_string())?
                }
                other => return Err(format!("a {other} reference has no image to import")),
            };
            let ext = if kind == "pdf" {
                "png".to_string()
            } else {
                std::path::Path::new(&entry.file)
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_else(|| "png".into())
            };
            let rel = format!("assets/{}.{ext}", &sha256_hex(&bytes)[..16]);
            crate::util::write_atomic(&ws.project.root.join(&rel), &bytes)
                .map_err(|e| e.to_string())?;
            done(
                json!({"asset": rel, "cite": cite, "use": "set this path as `asset` of an image and put the citation in its `cite` list"}),
            )
        }
        "code.read" => {
            let path = safe_deck_path(
                args.get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("deck/build.py"),
            )?;
            let text = std::fs::read_to_string(ws.project.root.join(&path))
                .map_err(|_| format!("{path} does not exist"))?;
            done(json!({"path": path, "content": text, "hash": sha256_hex(text.as_bytes())}))
        }
        "code.propose_edit" => {
            let path = safe_deck_path(
                args.get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("deck/build.py"),
            )?;
            let original = std::fs::read_to_string(ws.project.root.join(&path)).ok();
            let proposed =
                if let Some(content) = args.get("content").and_then(|c| c.as_str()) {
                    content.to_string()
                } else if let Some(edits) = args.get("edits").and_then(|e| e.as_array()) {
                    let mut text = original.clone().ok_or_else(|| {
                        format!("{path} does not exist; give `content` to create it")
                    })?;
                    for (i, edit) in edits.iter().enumerate() {
                        let (old, new) = (
                            edit["old"].as_str().unwrap_or(""),
                            edit["new"].as_str().unwrap_or(""),
                        );
                        match text.matches(old).count() {
                            1 => text = text.replacen(old, new, 1),
                            0 => {
                                return Err(format!(
                                    "edit {}: `old` was not found in {path}; read the file again",
                                    i + 1
                                ))
                            }
                            n => return Err(format!(
                                "edit {}: `old` matches {n} times in {path}; include more context",
                                i + 1
                            )),
                        }
                    }
                    text
                } else {
                    return Err("give either `edits` or `content`".into());
                };
            if original.as_deref() == Some(proposed.as_str()) {
                return Err("the proposed content is identical to the current file".into());
            }
            Ok(Prepared::Change(Change::CodeEdit {
                path,
                original,
                proposed,
            }))
        }
        "deck.build" => {
            let built = build_agent_deck(ws).await?;
            let diags = ws
                .check(AGENT_PPTX, None, None, false)
                .await
                .map_err(|e| e.to_string())?;
            let errors = diags.iter().filter(|d| d["severity"] == "error").count();
            done(
                json!({"built": true, "slides": built["element_map"].as_array().map(|a| a.iter().filter_map(|e| e["slide_id"].as_str()).collect::<BTreeSet<_>>().len()), "stdout": built["stdout"], "sandbox": built["sandbox"], "warnings": built["warnings"], "errors": errors, "findings": compact_diags(&diags)}),
            )
        }
        "deck.switch_mode" => {
            if store.read_deck().map_err(|e| e.to_string())?.mode == DeckMode::Code {
                return Err("the project is already in code generation mode".into());
            }
            Ok(Prepared::Change(Change::ModeSwitch {
                reason: str_arg(args, "reason")?.to_string(),
            }))
        }
        "skill.run_script" => {
            let skills = state.skills.as_ref().ok_or("no skills are installed")?;
            let name = str_arg(args, "name")?;
            let info = skills
                .list()
                .into_iter()
                .find(|s| s.name == name)
                .ok_or_else(|| format!("no skill {name:?}"))?;
            let script = str_arg(args, "script")?;
            let rel = if script.starts_with("scripts/") {
                script.to_string()
            } else {
                format!("scripts/{script}")
            };
            if rel.split('/').any(|c| c == "..") {
                return Err("the script path must stay inside the skill".into());
            }
            let out = ws
                .kernel
                .call_value("skill.run_script", json!({"script_abs": info.dir.join(&rel), "skill_dir_abs": info.dir, "args": args.get("args").cloned().unwrap_or(json!([])), "out_dir": format!(".present/cache/skills/{name}")}))
                .await
                .map_err(|e| e.to_string())?;
            let ok = out["exit_code"] == 0 && out["timed_out"] != true;
            Ok(Prepared::Done(ToolOutcome {
                ok,
                result: out,
                ..Default::default()
            }))
        }
        "export.run" => {
            let exported = ws
                .export(str_arg(args, "format")?, None)
                .await
                .map_err(|e| e.to_string())?;
            done(
                json!({"path": exported.path, "format": exported.format, "snapshot_id": exported.snapshot_id, "errors": exported.errors, "warnings": exported.warnings, "engine": exported.engine}),
            )
        }
        other => Err(format!("unknown tool {other}")),
    }
}

// ---- apply -----------------------------------------------------------------------------------

/// Write an accepted change. Returns what to tell the model.
pub async fn apply(
    ws: &Workspace,
    state: &mut ToolState,
    change: &Change,
) -> Result<Value, String> {
    let store = ws.deck();
    match change {
        Change::Outline { rows } => {
            let path = ws.project.root.join(".present").join("outline.json");
            crate::util::write_atomic(
                &path,
                crate::util::to_json_pretty(rows)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            Ok(
                json!({"accepted": true, "rows": rows.len(), "next": "build the slides in build mode, one per row"}),
            )
        }
        Change::SlideCreate {
            slide,
            after,
            section,
        } => {
            let hash = store
                .create_slide(
                    slide,
                    &Position {
                        after: after.clone(),
                        section: section.clone(),
                    },
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"created": slide.id, "hash": hash}))
        }
        Change::SlideUpdate {
            after,
            expected_hash,
            ..
        } => {
            let hash = store.update_slide(after, Some(expected_hash)).map_err(|e| match e {
                crate::deck_store::DeckError::Conflict(id) => format!("slide {id} was changed by the user while you were working; read it again and re-apply your change"),
                other => other.to_string(),
            })?;
            Ok(json!({"updated": after.id, "hash": hash}))
        }
        Change::SlideDelete { before } => {
            store.delete_slide(&before.id).map_err(|e| e.to_string())?;
            state.status.remove(before.id.as_str());
            Ok(json!({"deleted": before.id}))
        }
        Change::SlideMove { id, after, section } => {
            store
                .move_slide(
                    id,
                    &Position {
                        after: after.clone(),
                        section: section.clone(),
                    },
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"moved": id}))
        }
        Change::CodeEdit {
            path,
            original,
            proposed,
        } => {
            let full = ws.project.root.join(path);
            let current = std::fs::read_to_string(&full).ok();
            if &current != original {
                return Err(format!(
                    "{path} changed while you were working; read it again and re-apply your edit"
                ));
            }
            crate::util::write_atomic(&full, proposed.as_bytes()).map_err(|e| e.to_string())?;
            Ok(json!({"written": path}))
        }
        Change::ModeSwitch { .. } => {
            ws.snapshot(
                present_protocol::config::SnapshotReason::ModeSwitch,
                None,
                Some("before switching to code mode"),
            )
            .map_err(|e| e.to_string())?;
            let out = ws
                .kernel
                .call_value("deck.eject", json!({"out_dir": "deck"}))
                .await
                .map_err(|e| build_error(&e))?;
            if out["verified"] != true {
                let _ = std::fs::remove_file(ws.project.root.join("deck/build.py"));
                return Err(format!("the generated build script does not reproduce the deck, so the switch was not made:\n{}", out["diff"].as_str().unwrap_or("")));
            }
            store.set_mode(DeckMode::Code).map_err(|e| e.to_string())?;
            Ok(
                json!({"mode": "code", "script": out["script"], "note": "the deck is now deck/build.py; use code_read, code_propose_edit, and deck_build"}),
            )
        }
    }
}

/// Model-facing JSON for a map of string keys, used by the loop for scope results.
pub fn object(pairs: &[(&str, Value)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<Map<_, _>>(),
    )
}
