//! Deck evaluation tasks (`present/evals/tasks`), run through the one agent loop.
//!
//! A deck task's fixture names a template under the templates folder in `fixture/TEMPLATE`;
//! `fixture/sources/` holds files added as references, `fixture/deck/` an optional starting
//! deck, `fixture/assets/` its images, and `fixture/project.toml` an optional deck config.
//! The task becomes a fresh git repository (the project) with the deck under
//! `decks/<task name>/`; the agent runs in the task's mode with edits applied inside the
//! declared scope and code, skill scripts, and exports allowed. The checks inspect only the
//! finished deck, its build and checker report, and the turn's summary.

use super::EvalEnv;
use anyhow::{anyhow, Context, Result};
use fibration_core::agent::{Agent, AgentConfig, AutoApprover};
use fibration_core::service::decks::{self, DeckOptions};
use fibration_core::session::Session;
use fibration_core::tools::gate::Policy;
use fibration_core::tools::ToolContext;
use fibration_deck::tools::dispatch::{slide_text, title_of};
use fibration_deck::workspace::Workspace;
use fibration_deck_protocol::deck::{DeckMode, Slide};
use fibration_deck_protocol::evals::{Check, NativeKind, TaskFile};
use fibration_protocol::config::{EditPolicy, ProjectConfig, RunPolicy};
use fibration_protocol::evals::{CheckResult, Mode, TaskResult};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct DeckTask {
    pub dir: PathBuf,
    pub file: TaskFile,
}

/// Whether a task folder holds a deck task (its fixture names a template).
pub fn is_deck_task(dir: &Path) -> bool {
    let fixture = std::fs::read_to_string(dir.join("task.toml"))
        .ok()
        .and_then(|t| toml::from_str::<toml::Table>(&t).ok())
        .and_then(|t| {
            t.get("fixture")
                .and_then(|f| f.get("dir"))
                .and_then(|d| d.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "fixture".into());
    dir.join(fixture).join("TEMPLATE").is_file()
}

/// Load the deck tasks under `tasks_dir`, optionally only the one named `filter`.
pub fn load_tasks(tasks_dir: &Path, filter: Option<&str>) -> Result<Vec<DeckTask>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(tasks_dir)
        .with_context(|| format!("reading tasks dir {}", tasks_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("task.toml").is_file() && is_deck_task(p))
        .collect();
    dirs.sort();
    let mut out = Vec::new();
    for dir in dirs {
        let path = dir.join("task.toml");
        let file: TaskFile = toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("parsing {}", path.display()))?;
        if let Some(f) = filter {
            if file.task.name != f && dir.file_name().is_none_or(|n| n != f) {
                continue;
            }
        }
        out.push(DeckTask { dir, file });
    }
    if let (Some(f), true) = (filter, out.is_empty()) {
        return Err(anyhow!(
            "no deck task named `{f}` under {}",
            tasks_dir.display()
        ));
    }
    Ok(out)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(from).into_iter().flatten() {
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

/// The deck's slug: the task name when it is one, else `deck`.
fn slug_of(task: &DeckTask) -> String {
    let name = &task.file.task.name;
    if decks::valid_slug(name).is_ok() {
        name.clone()
    } else {
        "deck".into()
    }
}

/// Build the project from a task's fixture: a git repository with the fibration project files
/// and the deck under `decks/<slug>/`. Returns the locked slides of the starting deck (for
/// `locked_untouched`). References are added once the deck is open.
pub fn prepare(
    task: &DeckTask,
    templates_dir: &Path,
    repo: &Path,
) -> Result<BTreeMap<String, Slide>> {
    let fixture = task.dir.join(&task.file.fixture.dir);
    let template_name = std::fs::read_to_string(fixture.join("TEMPLATE"))
        .with_context(|| format!("{} needs a TEMPLATE file", fixture.display()))?;
    let template = templates_dir.join(template_name.trim());
    std::fs::create_dir_all(repo)?;
    fibration_core::init::init_repo(
        repo,
        &fibration_core::init::InitOptions {
            run_uv: false,
            git: false,
        },
    )?;
    let slug = slug_of(task);
    decks::create(
        repo,
        &slug,
        &task.file.task.name,
        &template,
        DeckMode::Structured,
    )
    .map_err(|e| anyhow!("creating the deck: {}", e.message))?;
    let root = decks::deck_root(repo, &slug);
    let store = fibration_deck::deck_store::DeckStore::new(&root);
    if fixture.join("project.toml").is_file() {
        std::fs::copy(
            fixture.join("project.toml"),
            root.join(".present").join("project.toml"),
        )?;
    }
    let mut locked = BTreeMap::new();
    if fixture.join("deck").is_dir() {
        let template_rel = store.read_deck()?.template;
        copy_dir(&fixture.join("deck"), &root.join("deck"))?;
        let mut deck = store.read_deck()?;
        deck.template = template_rel;
        store.write_deck(&deck)?;
        for id in deck.slide_ids() {
            let (slide, _) = store.read_slide(id)?;
            if slide.locked == Some(true) {
                locked.insert(id.to_string(), slide);
            }
        }
    }
    if fixture.join("assets").is_dir() {
        copy_dir(&fixture.join("assets"), &root.join("assets"))?;
    }
    Ok(locked)
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

/// Run every check of a task against the finished deck.
pub async fn run_checks(
    ws: &Workspace,
    checks: &[Check],
    locked: &BTreeMap<String, Slide>,
    unwanted_edits: u32,
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
                let detail = if n == 0 {
                    "opens clean".to_string()
                } else {
                    diags
                        .iter()
                        .find(|d| d["rule"] == "out.opens-clean")
                        .map(|d| d["message"].to_string())
                        .unwrap_or_default()
                };
                result(check, n == 0, detail)
            }
            Check::CheckerClean { max_warnings } => {
                let (errors, warnings) = (count("error"), count("warn"));
                let ok = errors == 0 && max_warnings.is_none_or(|m| warnings <= m as usize);
                let first = diags
                    .iter()
                    .find(|d| d["severity"] == "error")
                    .or_else(|| diags.iter().find(|d| d["severity"] == "warn"))
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
                result(
                    check,
                    ratio >= *min && dangling == 0,
                    format!("{cited} of {factual} factual items cited ({ratio:.2}); {dangling} dangling citations"),
                )
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
                unwanted_edits == 0,
                format!("{unwanted_edits} changes outside the declared scope"),
            ),
        };
        out.push(r);
    }
    out
}

fn failed(name: &str, kind: &str, detail: String, started: std::time::Instant) -> TaskResult {
    TaskResult {
        name: name.to_string(),
        passed: false,
        checks: vec![CheckResult {
            kind: kind.into(),
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
    }
}

/// Run one deck task end to end. Setup failures become a failed `setup` check.
pub async fn run_task(
    env: &EvalEnv,
    task: &DeckTask,
    templates_dir: &Path,
    options: &DeckOptions,
) -> TaskResult {
    let started = std::time::Instant::now();
    let name = task.file.task.name.clone();
    let work = match tempfile::Builder::new()
        .prefix(&format!("fibration-eval-{name}-"))
        .tempdir()
    {
        Ok(w) => w,
        Err(e) => return failed(&name, "setup", format!("work dir: {e}"), started),
    };
    match run_in(env, task, templates_dir, options, work.path(), started).await {
        Ok(r) => r,
        Err(e) => failed(&name, "setup", format!("{e:#}"), started),
    }
}

async fn run_in(
    env: &EvalEnv,
    task: &DeckTask,
    templates_dir: &Path,
    options: &DeckOptions,
    work: &Path,
    started: std::time::Instant,
) -> Result<TaskResult> {
    let name = task.file.task.name.clone();
    let repo = work.join("repo");
    let locked = prepare(task, templates_dir, &repo).context("fixture")?;
    let repo = std::fs::canonicalize(&repo)?;
    let task_home = work.join("home");
    std::fs::create_dir_all(&task_home)?;

    let mut project: ProjectConfig = fibration_core::config::load_project(&repo)?;
    // Deck evaluations run unattended: edits apply inside the declared scope.
    project.policy.edits = EditPolicy::Auto;
    project.policy.runs = RunPolicy::Auto;
    project.policy.max_steps = task.file.task.max_steps;
    let mode: Mode = task
        .file
        .task
        .mode
        .to_string()
        .parse()
        .map_err(|e: String| anyhow!(e))?;

    let session = Session::create(&task_home, &repo, "local", &env.model_name)?;
    let conv_id = "eval".to_string();
    let ctx = ToolContext::new(&repo, &task_home, session, &conv_id, project.clone());
    ctx.decks.set_options(options.clone());
    let slug = slug_of(task);
    let svc = ctx
        .decks
        .open(&slug)
        .await
        .map_err(|e| anyhow!("opening the deck: {}", e.message))?;
    let sources = task.dir.join(&task.file.fixture.dir).join("sources");
    if sources.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&sources)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        for file in files {
            svc.ws
                .add_reference(&file)
                .await
                .with_context(|| format!("adding {}", file.display()))?;
        }
    }
    // The prepared project is the baseline the turn's checkpoint and scope start from.
    fibration_core::checkpoint::git(&repo, &["init", "-q"], None)?;
    fibration_core::checkpoint::git(&repo, &["add", "-A"], None)?;
    fibration_core::checkpoint::git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "fixture"],
        None,
    )?;

    let policy = Policy::for_project(&repo, &project, mode)?;
    let config = AgentConfig::new(
        mode,
        &env.model_name,
        env.caps.clone(),
        task.file.task.max_steps,
    );
    let mut agent = Agent::new(
        env.model.clone(),
        ctx,
        policy,
        Arc::new(AutoApprover),
        config,
    );
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
                kind: "agent_turn".into(),
                passed: false,
                detail: format!("turn failed: {e:#}"),
            }],
        ),
        Err(_) => (
            Default::default(),
            vec![CheckResult {
                kind: "agent_turn".into(),
                passed: false,
                detail: format!("task timed out after {}s", task.file.task.timeout_s),
            }],
        ),
    };
    let unwanted = summary.unwanted_edits;
    let mut graded = run_checks(&svc.ws, &task.file.checks, &locked, unwanted).await;
    graded.append(&mut checks);
    agent.ctx.decks.shutdown().await;
    Ok(TaskResult {
        name,
        passed: graded.iter().all(|c| c.passed),
        checks: graded,
        steps: summary.steps,
        input_tokens: summary.input_tokens,
        output_tokens: summary.output_tokens,
        wall_s: started.elapsed().as_secs_f64(),
        unwanted_edits: unwanted,
        tool_errors: summary.tool_errors,
        tool_calls: summary.tool_calls,
    })
}
