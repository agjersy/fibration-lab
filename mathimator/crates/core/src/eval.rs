//! The evaluation harness (ARCHITECTURE 15, BUILD-SPEC WP-0.11): model-agnostic tasks with
//! declarative checks, in the shape of Personal Analyst's `task.toml`.
//!
//! A task is a directory with `task.toml` and, optionally, a starting `scene.py`:
//!
//! ```toml
//! [task]
//! name = "explore-family"
//! mode = "explore"                 # or "plan"
//! max_steps = 12
//! prompt = "Graph a*sin(b*x) ..."  # or: turns = ["first message", "second message"]
//!
//! [fixture]
//! scene = "scene.py"               # copied into the work directory as the starting scene
//!
//! [[checks]]
//! kind = "object"
//! object_kind = "explicit"
//! equals = "a*sin(b*x)"
//! ```
//!
//! Each task runs in a fresh work directory with a fresh engine, session and agent, so query
//! ids start at `q1` and nothing leaks between tasks. Checks read only what a user could see:
//! the notebook, the scene state, the claims with their computed badges, and the reply.
//!
//! | Check | Passes when |
//! |---|---|
//! | `runs_clean` | the final notebook replays with every cell running |
//! | `replay_deterministic` | a fresh engine replaying the final file gives the same scene hash |
//! | `scene_unchanged` | the scene hash equals the starting scene's |
//! | `cells` | the notebook has between `min` and `max` cells |
//! | `param` | a parameter `name` is declared |
//! | `object` | at least `min_count` objects of `object_kind` exist; with `equals`, one of them has an expression the engine proves equal to it |
//! | `notes` | at least `min_count` notes exist (matching `matches` when given) |
//! | `markers` | sampling the explicit curves over `x`/`y` finds at least `min_count` markers of `marker` |
//! | `claims` | at least `min_count` claims have a status in `status` (and match `matches` when given); with `max_unverified`, no more than that many are unverified |
//! | `reply_matches` | the model's prose matches the regular expression `pattern` (case-insensitive) |
//! | `ended` | the last turn ended as `end` (`done` by default) |

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wb_providers::ModelClient;

use crate::agent::{Agent, AgentConfig, Claim, ClaimStatus, Mode, TurnEnd};
use crate::engine::EngineConfig;
use crate::replay::ReplayOptions;
use crate::session::{SceneSession, SessionConfig};

#[derive(Clone, Debug, Deserialize)]
pub struct TaskFile {
    pub task: TaskSpec,
    #[serde(default)]
    pub fixture: Fixture,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TaskSpec {
    pub name: String,
    #[serde(default = "default_mode")]
    pub mode: Mode,
    #[serde(default = "default_steps")]
    pub max_steps: usize,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub turns: Vec<String>,
}

fn default_mode() -> Mode {
    Mode::Explore
}

fn default_steps() -> usize {
    16
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Fixture {
    /// A scene file in the task directory to start from.
    #[serde(default)]
    pub scene: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    RunsClean,
    ReplayDeterministic,
    SceneUnchanged,
    Cells {
        #[serde(default)]
        min: Option<usize>,
        #[serde(default)]
        max: Option<usize>,
    },
    Param {
        name: String,
    },
    Object {
        object_kind: String,
        #[serde(default = "one")]
        min_count: usize,
        #[serde(default)]
        equals: Option<String>,
    },
    Notes {
        #[serde(default = "one")]
        min_count: usize,
        #[serde(default)]
        matches: Option<String>,
    },
    Markers {
        marker: String,
        #[serde(default = "one")]
        min_count: usize,
        #[serde(default = "default_window")]
        x: [f64; 2],
        #[serde(default = "default_window")]
        y: [f64; 2],
    },
    Claims {
        #[serde(default)]
        status: Vec<ClaimStatus>,
        #[serde(default = "one")]
        min_count: usize,
        #[serde(default)]
        matches: Option<String>,
        #[serde(default)]
        max_unverified: Option<usize>,
    },
    ReplyMatches {
        pattern: String,
    },
    Ended {
        #[serde(default = "default_end")]
        end: String,
    },
}

fn one() -> usize {
    1
}

fn default_window() -> [f64; 2] {
    [-10.0, 10.0]
}

fn default_end() -> String {
    "done".into()
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckResult {
    pub kind: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskResult {
    pub task: String,
    pub model: String,
    pub passed: bool,
    pub checks: Vec<CheckResult>,
    pub steps: usize,
    pub cells_kept: usize,
    pub cells_refused: usize,
    pub claims: Vec<Claim>,
    pub reply: String,
    pub end: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Where the task ran; the final `scene.py` and `events.jsonl` are there.
    pub work_dir: PathBuf,
}

/// What a run needs besides the task.
pub struct EvalOptions {
    pub engine: EngineConfig,
    pub replay: ReplayOptions,
    /// Work directories are created under this directory, one per task.
    pub work_root: PathBuf,
}

pub fn load_task(dir: &Path) -> Result<TaskFile> {
    let path = dir.join("task.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let task: TaskFile =
        toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    if task.task.prompt.is_none() && task.task.turns.is_empty() {
        bail!("{}: a task needs `prompt` or `turns`", path.display());
    }
    Ok(task)
}

/// The task directories under `root` (those holding a `task.toml`), sorted by name.
pub fn list_tasks(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("task.toml").is_file())
        .collect();
    dirs.sort();
    Ok(dirs)
}

fn matches(pattern: &str, text: &str) -> Result<bool> {
    Ok(RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .with_context(|| format!("bad pattern `{pattern}`"))?
        .is_match(text))
}

fn end_name(end: &TurnEnd) -> String {
    match end {
        TurnEnd::Done => "done".into(),
        TurnEnd::StepLimit => "step_limit".into(),
        TurnEnd::ModelError(m) => format!("model_error: {m}"),
    }
}

fn session_config(opts: &EvalOptions, dir: &Path, name: &str) -> SessionConfig {
    SessionConfig {
        engine: opts.engine.clone(),
        replay: opts.replay.clone(),
        // A short path: Unix socket paths are limited to about 100 bytes.
        socket: dir.join(format!("s-{name}/a.sock")),
        call_log: Some(dir.join("calls.jsonl")),
    }
}

/// Run one task with `client` and grade it.
pub async fn run_task(
    task_dir: &Path,
    client: Arc<dyn ModelClient>,
    opts: &EvalOptions,
) -> Result<TaskResult> {
    let file = load_task(task_dir)?;
    let spec = &file.task;
    let work = opts.work_root.join(&spec.name);
    if work.exists() {
        std::fs::remove_dir_all(&work).with_context(|| format!("clearing {}", work.display()))?;
    }
    std::fs::create_dir_all(&work)?;
    let scene = work.join("scene.py");
    if let Some(fixture) = &file.fixture.scene {
        std::fs::copy(task_dir.join(fixture), &scene)
            .with_context(|| format!("copying the fixture scene {fixture}"))?;
    }

    let mut session = SceneSession::open(&scene, session_config(opts, &work, "run")).await?;
    if !session.last().ok() {
        bail!("task {}: the fixture scene does not run", spec.name);
    }
    let start_hash = session.last().hash.clone();
    let model = client.model_id().to_string();
    let mut agent = Agent::new(
        client,
        AgentConfig {
            mode: spec.mode,
            max_steps: spec.max_steps,
            transcript: Some(work.join("events.jsonl")),
            ..AgentConfig::default()
        },
    );

    let turns: Vec<String> = match (&spec.prompt, spec.turns.is_empty()) {
        (Some(p), true) => vec![p.trim().to_string()],
        _ => spec.turns.iter().map(|t| t.trim().to_string()).collect(),
    };
    let mut claims: Vec<Claim> = Vec::new();
    let mut reply = String::new();
    let (mut steps, mut kept, mut refused, mut tokens_in, mut tokens_out) = (0, 0, 0, 0, 0);
    let mut end = TurnEnd::Done;
    for turn in &turns {
        let report = agent.turn(&mut session, turn).await?;
        claims.extend(report.claims);
        if !report.text.is_empty() {
            if !reply.is_empty() {
                reply.push_str("\n\n");
            }
            reply.push_str(&report.text);
        }
        steps += report.steps;
        kept += report.cells.len();
        refused += report.refused;
        tokens_in += report.input_tokens;
        tokens_out += report.output_tokens;
        end = report.end;
        if end != TurnEnd::Done {
            break;
        }
    }

    let mut results = Vec::new();
    for check in &file.checks {
        let (kind, outcome) = grade(
            check,
            &session,
            &claims,
            &reply,
            &end,
            &start_hash,
            opts,
            &work,
        )
        .await;
        let (passed, detail) = match outcome {
            Ok(pair) => pair,
            Err(e) => (false, format!("the check could not run: {e:#}")),
        };
        results.push(CheckResult {
            kind,
            passed,
            detail,
        });
    }
    session.close().await.ok();

    Ok(TaskResult {
        task: spec.name.clone(),
        model,
        passed: results.iter().all(|c| c.passed),
        checks: results,
        steps,
        cells_kept: kept,
        cells_refused: refused,
        claims,
        reply,
        end: end_name(&end),
        input_tokens: tokens_in,
        output_tokens: tokens_out,
        work_dir: work,
    })
}

#[allow(clippy::too_many_arguments)]
async fn grade(
    check: &Check,
    session: &SceneSession,
    claims: &[Claim],
    reply: &str,
    end: &TurnEnd,
    start_hash: &str,
    opts: &EvalOptions,
    work: &Path,
) -> (String, Result<(bool, String)>) {
    let state = session.state();
    let objects = state["objects"].as_array().cloned().unwrap_or_default();
    let kind = serde_json::to_value(KindOf(check))
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let outcome: Result<(bool, String)> = async {
        Ok(match check {
            Check::RunsClean => {
                let failing = session.last().results.iter().find(|r| r.error.is_some());
                match failing {
                    None => (
                        true,
                        format!("{} cells ran", session.notebook().cells.len()),
                    ),
                    Some(r) => (false, format!("cell {} failed", r.id)),
                }
            }
            Check::ReplayDeterministic => {
                // A second engine with an empty memo, replaying the file as it is on disk.
                let copy = work.join("replay-check.py");
                std::fs::copy(session.path(), &copy)?;
                let mut cfg = session_config(opts, work, "chk");
                cfg.engine.memo_dir = None;
                cfg.call_log = None;
                let fresh = SceneSession::open(&copy, cfg).await?;
                let hash = fresh.last().hash.clone();
                fresh.close().await.ok();
                std::fs::remove_file(&copy).ok();
                let same = hash == session.last().hash;
                (
                    same,
                    if same {
                        "same scene hash".into()
                    } else {
                        format!("{hash} != {}", session.last().hash)
                    },
                )
            }
            Check::SceneUnchanged => {
                let same = session.last().hash == start_hash;
                (
                    same,
                    if same {
                        "scene hash unchanged".into()
                    } else {
                        "the scene changed".into()
                    },
                )
            }
            Check::Cells { min, max } => {
                let n = session.notebook().cells.len();
                let ok = min.is_none_or(|m| n >= m) && max.is_none_or(|m| n <= m);
                (ok, format!("{n} cells"))
            }
            Check::Param { name } => {
                let ok = state["params"].get(name).is_some();
                (
                    ok,
                    if ok {
                        format!("{name} is declared")
                    } else {
                        format!("no parameter {name}")
                    },
                )
            }
            Check::Object {
                object_kind,
                min_count,
                equals,
            } => {
                let of_kind: Vec<&Value> = objects
                    .iter()
                    .filter(|o| o["kind"] == object_kind.as_str())
                    .collect();
                if of_kind.len() < *min_count {
                    return Ok((
                        false,
                        format!("{} {object_kind} objects, need {min_count}", of_kind.len()),
                    ));
                }
                match equals {
                    None => (true, format!("{} {object_kind} objects", of_kind.len())),
                    Some(target) => {
                        let mut seen = Vec::new();
                        for o in &of_kind {
                            let Some(expr) = o["exprs"].as_object().and_then(|m| {
                                m.get("f")
                                    .or_else(|| m.get("r"))
                                    .or_else(|| m.get("relation"))
                            }) else {
                                continue;
                            };
                            let text = expr["text"].as_str().unwrap_or("");
                            seen.push(text.to_string());
                            let reply = session
                                .query("equals", json!({"a": text, "b": target}), "attach")
                                .await?;
                            if reply["value"]["truth"] == "true" {
                                return Ok((
                                    true,
                                    format!("{} is {text}", o["id"].as_str().unwrap_or("?")),
                                ));
                            }
                        }
                        (
                            false,
                            format!(
                                "no {object_kind} object equals {target}; found {}",
                                seen.join(", ")
                            ),
                        )
                    }
                }
            }
            Check::Notes {
                min_count,
                matches: pattern,
            } => {
                let notes = state["notes"].as_array().cloned().unwrap_or_default();
                let mut n = 0;
                for note in &notes {
                    let text = format!(
                        "{} {}",
                        note["spec"]["text"].as_str().unwrap_or(""),
                        note["spec"]["latex"].as_str().unwrap_or("")
                    );
                    if pattern
                        .as_deref()
                        .map(|p| matches(p, &text))
                        .transpose()?
                        .unwrap_or(true)
                    {
                        n += 1;
                    }
                }
                (n >= *min_count, format!("{n} matching notes"))
            }
            Check::Markers {
                marker,
                min_count,
                x,
                y,
            } => {
                let mut n = 0;
                let mut statuses = Vec::new();
                for o in objects.iter().filter(|o| o["kind"] == "explicit") {
                    let out = session
                        .engine_request(
                            "sample",
                            json!({"object": o["id"], "viewport": {"x": x, "y": y}}),
                        )
                        .await?;
                    statuses.push(out["status"].as_str().unwrap_or("?").to_string());
                    n += out["markers"]
                        .as_array()
                        .map(|m| m.iter().filter(|m| m["kind"] == marker.as_str()).count())
                        .unwrap_or(0);
                }
                (
                    n >= *min_count,
                    format!(
                        "{n} {marker} markers on {} curves ({})",
                        statuses.len(),
                        statuses.join(", ")
                    ),
                )
            }
            Check::Claims {
                status,
                min_count,
                matches: pattern,
                max_unverified,
            } => {
                let mut n = 0;
                for c in claims {
                    let status_ok = status.is_empty() || status.contains(&c.status);
                    if status_ok
                        && pattern
                            .as_deref()
                            .map(|p| matches(p, &c.text))
                            .transpose()?
                            .unwrap_or(true)
                    {
                        n += 1;
                    }
                }
                let unverified = claims
                    .iter()
                    .filter(|c| c.status == ClaimStatus::Unverified)
                    .count();
                let ok = n >= *min_count && max_unverified.is_none_or(|m| unverified <= m);
                (
                    ok,
                    format!(
                        "{n} matching claims of {}, {unverified} unverified",
                        claims.len()
                    ),
                )
            }
            Check::ReplyMatches { pattern } => {
                let ok = matches(pattern, reply)?;
                (
                    ok,
                    if ok {
                        "the reply matches".into()
                    } else {
                        format!("the reply does not match /{pattern}/")
                    },
                )
            }
            Check::Ended { end: wanted } => {
                let got = end_name(end);
                (got == *wanted, format!("ended as {got}"))
            }
        })
    }
    .await;
    (kind, outcome)
}

/// Serializes a check as just its `kind` tag.
struct KindOf<'a>(&'a Check);

impl Serialize for KindOf<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(match self.0 {
            Check::RunsClean => "runs_clean",
            Check::ReplayDeterministic => "replay_deterministic",
            Check::SceneUnchanged => "scene_unchanged",
            Check::Cells { .. } => "cells",
            Check::Param { .. } => "param",
            Check::Object { .. } => "object",
            Check::Notes { .. } => "notes",
            Check::Markers { .. } => "markers",
            Check::Claims { .. } => "claims",
            Check::ReplyMatches { .. } => "reply_matches",
            Check::Ended { .. } => "ended",
        })
    }
}

/// Write `results` as a scorecard (`scorecard.json`) under `dir`.
pub fn write_scorecard(dir: &Path, results: &[TaskResult]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("scorecard.json");
    let model = results.first().map(|r| r.model.clone()).unwrap_or_default();
    let body = json!({
        "schema": 1,
        "model": model,
        "passed": results.iter().filter(|r| r.passed).count(),
        "total": results.len(),
        "tasks": results,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&body)?)?;
    Ok(path)
}

/// A one-line-per-check text report.
pub fn report(result: &TaskResult) -> String {
    let mut out = format!(
        "{} {}  ({} steps, {} cells kept, {} refused, {} claims)\n",
        if result.passed { "PASS" } else { "FAIL" },
        result.task,
        result.steps,
        result.cells_kept,
        result.cells_refused,
        result.claims.len()
    );
    for c in &result.checks {
        out.push_str(&format!(
            "  {} {:<22} {}\n",
            if c.passed { "ok  " } else { "FAIL" },
            c.kind,
            c.detail
        ));
    }
    out
}

/// Errors for a task directory that is not one.
pub fn require_task(dir: &Path) -> Result<()> {
    if dir.join("task.toml").is_file() {
        Ok(())
    } else {
        Err(anyhow!("{} has no task.toml", dir.display()))
    }
}
