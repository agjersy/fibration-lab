//! Evaluation harness (PROTOCOL 12): fixture preparation, running the agent with
//! `AutoApprover`, check executors, and scorecards. Structure ported from Personal Analyst
//! `crates/core/src/evals.rs` at commit f229e4d.
//!
//! Fixture layout: `fixture/TEMPLATE` names a template under `evals/templates`,
//! `fixture/sources/` holds files added as references, `fixture/deck/` is an optional starting
//! deck, and `fixture/project.toml` an optional project configuration.

use crate::agent::dispatch::{slide_text, title_of};
use crate::agent::tools::Mode;
use crate::agent::AutoApprover;
use crate::project::Project;
use crate::session::{build_agent, policy_for, ResolvedModel};
use crate::workspace::{OpenOptions, Workspace};
use analyst_deck_protocol::deck::{DeckMode, Slide};
use analyst_deck_protocol::evals::{
    Check, CheckResult, NativeKind, Scorecard, ScorecardSummary, TaskFile, TaskResult,
};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use wb_policy::{EditGate, Level};

#[derive(Debug, Clone)]
pub struct LoadedTask {
    pub dir: PathBuf,
    pub file: TaskFile,
}

/// Load `<tasks_dir>/*/task.toml`, optionally only the task named `filter`.
pub fn load_tasks(tasks_dir: &Path, filter: Option<&str>) -> Result<Vec<LoadedTask>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(tasks_dir)
        .with_context(|| format!("reading tasks dir {}", tasks_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut out = Vec::new();
    for dir in dirs {
        let path = dir.join("task.toml");
        if !path.is_file() {
            continue;
        }
        let file: TaskFile = toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("parsing {}", path.display()))?;
        if let Some(f) = filter {
            if file.task.name != f && dir.file_name().is_none_or(|n| n != f) {
                continue;
            }
        }
        out.push(LoadedTask { dir, file });
    }
    if let (Some(f), true) = (filter, out.is_empty()) {
        return Err(anyhow!("no task named `{f}` under {}", tasks_dir.display()));
    }
    Ok(out)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = entry.path().strip_prefix(from)?;
        let target = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Build a fresh project from a task's fixture. Returns the project root and the locked slides
/// of the starting deck (for `locked_untouched`).
pub async fn prepare(
    task: &LoadedTask,
    templates_dir: &Path,
    work: &Path,
    options: &OpenOptions,
) -> Result<(Arc<Workspace>, BTreeMap<String, Slide>)> {
    let fixture = task.dir.join(&task.file.fixture.dir);
    let template_name = std::fs::read_to_string(fixture.join("TEMPLATE"))
        .with_context(|| format!("{} needs a TEMPLATE file", fixture.display()))?;
    let template = templates_dir.join(template_name.trim());
    let root = work.join(&task.file.task.name);
    let project = Project::create(&root, &task.file.task.name, &template, DeckMode::Structured)?;
    if fixture.join("project.toml").is_file() {
        std::fs::copy(
            fixture.join("project.toml"),
            root.join(".present").join("project.toml"),
        )?;
    }
    let mut locked = BTreeMap::new();
    if fixture.join("deck").is_dir() {
        let template_rel = project.deck().read_deck()?.template;
        copy_dir(&fixture.join("deck"), &root.join("deck"))?;
        let mut deck = project.deck().read_deck()?;
        deck.template = template_rel;
        project.deck().write_deck(&deck)?;
        for id in deck.slide_ids() {
            let (slide, _) = project.deck().read_slide(id)?;
            if slide.locked == Some(true) {
                locked.insert(id.to_string(), slide);
            }
        }
    }
    if fixture.join("assets").is_dir() {
        copy_dir(&fixture.join("assets"), &root.join("assets"))?;
    }
    let ws = Arc::new(Workspace::open(&root, options).await?);
    let sources = fixture.join("sources");
    if sources.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&sources)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        for file in files {
            ws.add_reference(&file)
                .await
                .with_context(|| format!("adding {}", file.display()))?;
        }
    }
    Ok((ws, locked))
}

fn result(check: &Check, passed: bool, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        kind: check.kind().to_string(),
        passed,
        detail: detail.into(),
    }
}

fn has_digit(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
}

/// Counts for one text: factual paragraphs, cited ones among them, and cited numeric runs.
fn text_stats(text: &Value, is_title: bool) -> (usize, usize, usize) {
    let (mut factual, mut cited, mut numeric_runs) = (0, 0, 0);
    for para in text["paragraphs"].as_array().into_iter().flatten() {
        let runs: Vec<&Value> = para["runs"].as_array().into_iter().flatten().collect();
        let plain: String = runs.iter().filter_map(|r| r["t"].as_str()).collect();
        let has_cite = runs
            .iter()
            .any(|r| r["cite"].as_array().is_some_and(|a| !a.is_empty()));
        let waived = runs
            .iter()
            .any(|r| r["cite"].as_array().is_some_and(|a| a.is_empty()));
        if !is_title && has_digit(&plain) && !waived {
            factual += 1;
            if has_cite {
                cited += 1;
            }
        }
        if has_cite {
            numeric_runs += runs
                .iter()
                .filter(|r| r["t"].as_str().is_some_and(has_digit))
                .count();
        }
    }
    (factual, cited, numeric_runs)
}

/// Factual items (paragraphs with figures, charts) and how many carry a citation, plus the
/// number of cited numeric runs.
fn citation_stats(slides: &[Slide]) -> (usize, usize, usize) {
    let (mut factual, mut cited, mut numeric_runs) = (0, 0, 0);
    for slide in slides {
        let v = serde_json::to_value(slide).unwrap_or(Value::Null);
        for (key, item) in v["placeholders"].as_object().into_iter().flatten() {
            match item["type"].as_str() {
                Some("text") => {
                    let (f, c, n) = text_stats(&item["text"], key == "title");
                    factual += f;
                    cited += c;
                    numeric_runs += n;
                }
                Some("chart") => {
                    factual += 1;
                    let src = &item["chart"]["source"];
                    if src["cite"].is_string()
                        || src["cites"].as_array().is_some_and(|a| !a.is_empty())
                    {
                        cited += 1;
                    }
                }
                _ => {}
            }
        }
    }
    (factual, cited, numeric_runs)
}

/// Run every check of a task against the finished project.
pub async fn run_checks(
    ws: &Workspace,
    checks: &[Check],
    locked: &BTreeMap<String, Slide>,
    out_of_scope: u32,
) -> Vec<CheckResult> {
    let pptx = "exports/eval.pptx";
    let built = ws.build(pptx, "", None).await;
    let diags = match &built {
        Ok(_) => ws.check(pptx, None, None, true).await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let build_error = built.as_ref().err().map(|e| e.to_string());
    let element_map: Vec<Value> = built
        .as_ref()
        .ok()
        .and_then(|b| b["element_map"].as_array().cloned())
        .unwrap_or_default();
    let (deck, slides) = match ws.deck().read_all() {
        Ok(pair) => (Some(pair.0), pair.1),
        Err(_) => (ws.deck().read_deck().ok(), Vec::new()),
    };
    let code_mode = deck.as_ref().is_some_and(|d| d.mode == DeckMode::Code);
    let count = |sev: &str| diags.iter().filter(|d| d["severity"] == sev).count();
    let rule = |name: &str| diags.iter().filter(|d| d["rule"] == name).count();
    let mut out = Vec::new();
    for check in checks {
        if let Some(err) = &build_error {
            out.push(result(
                check,
                false,
                format!("the deck did not build: {err}"),
            ));
            continue;
        }
        let r = match check {
            Check::OpensClean => {
                let n = rule("out.opens-clean");
                result(
                    check,
                    n == 0,
                    if n == 0 {
                        "opens clean".to_string()
                    } else {
                        diags
                            .iter()
                            .find(|d| d["rule"] == "out.opens-clean")
                            .map(|d| d["message"].to_string())
                            .unwrap_or_default()
                    },
                )
            }
            Check::CheckerClean { max_warnings } => {
                let (errors, warnings) = (count("error"), count("warn"));
                let ok = errors == 0 && max_warnings.is_none_or(|m| warnings <= m as usize);
                let first = diags
                    .iter()
                    .find(|d| d["severity"] == "error")
                    .map(|d| {
                        format!(
                            ": [{}] {}",
                            d["rule"].as_str().unwrap_or(""),
                            d["message"].as_str().unwrap_or("")
                        )
                    })
                    .unwrap_or_default();
                result(
                    check,
                    ok,
                    format!("{errors} errors, {warnings} warnings{first}"),
                )
            }
            Check::SlideCount { min, max, exact } => {
                let n = if code_mode {
                    element_map
                        .iter()
                        .filter_map(|e| e["slide_id"].as_str())
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                } else {
                    slides.iter().filter(|s| s.hidden != Some(true)).count()
                } as u32;
                let ok = exact.is_none_or(|e| n == e)
                    && min.is_none_or(|m| n >= m)
                    && max.is_none_or(|m| n <= m);
                result(check, ok, format!("{n} visible slides"))
            }
            Check::LayoutUsed { layout, min } => {
                let n = slides.iter().filter(|s| &s.layout == layout).count() as u32;
                result(
                    check,
                    n >= min.unwrap_or(1),
                    format!("layout {layout:?} used {n} times"),
                )
            }
            Check::UsesNative { element, min } => {
                let kind = match element {
                    NativeKind::Chart => "chart",
                    NativeKind::Table => "table",
                    NativeKind::Diagram => "group",
                };
                let n = element_map.iter().filter(|e| e["kind"] == kind).count() as u32;
                result(
                    check,
                    n >= min.unwrap_or(1),
                    format!("{n} native {kind} elements"),
                )
            }
            Check::CitationCoverage { min } => {
                let (factual, cited, _) = citation_stats(&slides);
                let ratio = if factual == 0 {
                    1.0
                } else {
                    cited as f64 / factual as f64
                };
                let dangling = rule("src.dangling");
                result(check, ratio >= *min && dangling == 0, format!("{cited} of {factual} factual items cited ({ratio:.2}); {dangling} dangling citations"))
            }
            Check::NumbersVerified { min } => {
                let (_, _, numeric) = citation_stats(&slides);
                let red = rule("src.numbers");
                let ratio = if numeric == 0 {
                    1.0
                } else {
                    1.0 - (red as f64 / numeric as f64).min(1.0)
                };
                result(
                    check,
                    ratio >= *min,
                    format!(
                        "{red} unverified numbers in {numeric} cited numeric runs ({ratio:.2})"
                    ),
                )
            }
            Check::TextMatches { slide, regex } => match regex::Regex::new(regex) {
                Ok(re) => {
                    let hit = slides
                        .iter()
                        .filter(|s| slide.as_ref().is_none_or(|id| &s.id == id))
                        .any(|s| re.is_match(&slide_text(s)) || re.is_match(&title_of(s)));
                    result(
                        check,
                        hit,
                        if hit {
                            "matched"
                        } else {
                            "no slide text matches"
                        },
                    )
                }
                Err(e) => result(check, false, format!("bad regex: {e}")),
            },
            Check::LockedUntouched => {
                let changed: Vec<&String> = locked
                    .iter()
                    .filter(|(id, before)| {
                        !slides
                            .iter()
                            .any(|s| s.id.as_str() == id.as_str() && &s == before)
                    })
                    .map(|(id, _)| id)
                    .collect();
                result(
                    check,
                    changed.is_empty(),
                    if changed.is_empty() {
                        format!("{} locked slides unchanged", locked.len())
                    } else {
                        format!("locked slides changed: {changed:?}")
                    },
                )
            }
            Check::Mode { mode } => {
                let actual = deck.as_ref().map(|d| d.mode);
                result(
                    check,
                    actual == Some(*mode),
                    format!(
                        "deck mode is {}",
                        actual.map(|m| m.to_string()).unwrap_or_default()
                    ),
                )
            }
            Check::ExportExists { format } => {
                let ext = match format.as_str() {
                    "notes-docx" => "docx",
                    other => other,
                };
                let found = std::fs::read_dir(ws.project.root.join("exports"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|e| {
                        let p = e.path();
                        p.file_name().is_some_and(|n| n != "eval.pptx")
                            && p.extension().is_some_and(|x| x == ext)
                            && p.metadata().map(|m| m.len() > 0).unwrap_or(false)
                    });
                result(
                    check,
                    found,
                    if found {
                        format!("an exported .{ext} exists")
                    } else {
                        format!("no exported .{ext} file")
                    },
                )
            }
            Check::EditsWithinScope => result(
                check,
                out_of_scope == 0,
                format!("{out_of_scope} changes outside the declared scope"),
            ),
        };
        out.push(r);
    }
    out
}

/// Run one task end to end.
pub async fn run_task(
    task: &LoadedTask,
    model: &ResolvedModel,
    templates_dir: &Path,
    work: &Path,
    options: &OpenOptions,
) -> TaskResult {
    let started = std::time::Instant::now();
    let name = task.file.task.name.clone();
    let fail = |detail: String, started: std::time::Instant| TaskResult {
        name: name.clone(),
        passed: false,
        checks: vec![CheckResult {
            kind: "setup".into(),
            passed: false,
            detail,
        }],
        steps: 0,
        input_tokens: 0,
        output_tokens: 0,
        wall_s: started.elapsed().as_secs_f64(),
        unwanted_edits: 0,
        tool_errors: 0,
        tool_calls: 0,
    };
    let (ws, locked) = match prepare(task, templates_dir, work, options).await {
        Ok(v) => v,
        Err(e) => return fail(format!("fixture: {e:#}"), started),
    };
    let mode: Mode = task
        .file
        .task
        .mode
        .to_string()
        .parse()
        .unwrap_or(Mode::Build);
    let policy = match ws.project.config().and_then(|c| policy_for(&c)) {
        // Evaluations run unattended: edits apply inside the declared scope, the rest is automatic.
        Ok(p) => p
            .with_edits(EditGate::Auto)
            .with_class("code", Level::Auto)
            .with_class("skills_scripts", Level::Auto)
            .with_class("export", Level::Auto),
        Err(e) => return fail(format!("policy: {e:#}"), started),
    };
    let mut agent = match build_agent(
        ws.clone(),
        model,
        policy,
        Arc::new(AutoApprover),
        mode,
        "eval",
        task.file.task.max_steps,
    ) {
        Ok(a) => a,
        Err(e) => return fail(format!("agent: {e:#}"), started),
    };
    let turn = tokio::time::timeout(
        Duration::from_secs(task.file.task.timeout_s),
        agent.run_turn(&task.file.task.prompt),
    )
    .await;
    let (summary, mut checks) = match turn {
        Ok(Ok(summary)) => (summary, Vec::new()),
        Ok(Err(e)) => (
            Default::default(),
            vec![CheckResult {
                kind: "turn".into(),
                passed: false,
                detail: format!("{e:#}"),
            }],
        ),
        Err(_) => (
            Default::default(),
            vec![CheckResult {
                kind: "turn".into(),
                passed: false,
                detail: format!("timed out after {} s", task.file.task.timeout_s),
            }],
        ),
    };
    checks.extend(run_checks(&ws, &task.file.checks, &locked, summary.out_of_scope).await);
    ws.close().await;
    TaskResult {
        name,
        passed: checks.iter().all(|c| c.passed),
        checks,
        steps: summary.steps,
        input_tokens: summary.input_tokens,
        output_tokens: summary.output_tokens,
        wall_s: started.elapsed().as_secs_f64(),
        unwanted_edits: summary.out_of_scope,
        tool_errors: summary.tool_errors,
        tool_calls: summary.tool_calls,
    }
}

pub fn scorecard(model: &str, tasks: Vec<TaskResult>) -> Scorecard {
    let n = tasks.len().max(1) as f64;
    let calls: u32 = tasks.iter().map(|t| t.tool_calls).sum();
    let summary = ScorecardSummary {
        pass_rate: tasks.iter().filter(|t| t.passed).count() as f64 / n,
        unwanted_edit_rate: tasks.iter().filter(|t| t.unwanted_edits > 0).count() as f64 / n,
        tool_error_rate: if calls == 0 {
            0.0
        } else {
            tasks.iter().map(|t| t.tool_errors).sum::<u32>() as f64 / calls as f64
        },
    };
    Scorecard {
        model: model.to_string(),
        tasks,
        summary,
    }
}

pub fn write_scorecard(dir: &Path, card: &Scorecard) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let safe: String = card
        .model
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}-{stamp}.json"));
    crate::util::write_atomic(&path, crate::util::to_json_pretty(card)?.as_bytes())?;
    Ok(path)
}

pub fn render_scorecard(card: &Scorecard) -> String {
    let mut out = format!("model: {}\n", card.model);
    for t in &card.tasks {
        out.push_str(&format!(
            "{} {}  ({} steps, {} tool calls, {:.1} s)\n",
            if t.passed { "PASS" } else { "FAIL" },
            t.name,
            t.steps,
            t.tool_calls,
            t.wall_s
        ));
        for c in &t.checks {
            out.push_str(&format!(
                "    {} {:<18} {}\n",
                if c.passed { "ok  " } else { "FAIL" },
                c.kind,
                c.detail
            ));
        }
    }
    out.push_str(&format!(
        "pass rate {:.0}%, unwanted-edit rate {:.0}%, tool-error rate {:.0}%\n",
        card.summary.pass_rate * 100.0,
        card.summary.unwanted_edit_rate * 100.0,
        card.summary.tool_error_rate * 100.0
    ));
    out
}
