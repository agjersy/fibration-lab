//! Commands that work on a scene through a session: `chat`, `chips`, `chip`, `set`.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use math_core::agent::{Agent, AgentConfig, AgentEvent, ClaimStatus, Mode, TurnEnd, TurnReport};
use math_core::chips::ChipKind;
use math_core::{config, Applied, EngineConfig, ReplayOptions, SceneSession, SessionConfig};

/// A session for one command. The temporary directory holds the attach socket and must
/// outlive the session.
pub struct Opened {
    pub session: SceneSession,
    _socket_dir: tempfile::TempDir,
}

pub async fn open(
    scene: &Path,
    engine: EngineConfig,
    project_python: Option<PathBuf>,
) -> Result<Opened> {
    let scene_dir = scene
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let cache = scene_dir.join(".cache");
    let socket_dir = tempfile::Builder::new()
        .prefix("fibration-math-")
        .tempdir()?;
    let mut replay = ReplayOptions::default();
    if let Some(p) = project_python {
        replay.project_python = p;
    }
    let config = SessionConfig {
        engine: engine.with_memo_dir(cache.join("memo")),
        replay,
        socket: socket_dir.path().join("sock/attach.sock"),
        call_log: Some(cache.join("calls.jsonl")),
    };
    let session = SceneSession::open(scene, config).await?;
    if session.isolation().starts_with("none") {
        eprintln!(
            "warning: network not isolated for the engine and cells ({})",
            session.isolation()
        );
    }
    if !session.last().ok() {
        // The scene on disk does not run; edits on top of it could not be validated.
        for r in &session.last().results {
            if let Some(e) = &r.error {
                eprintln!("{}: {}: {}", r.id, e.kind, e.message);
            }
        }
        bail!(
            "{} does not run as it is; fix it first (`fibration-math run` shows the error)",
            scene.display()
        );
    }
    Ok(Opened {
        session,
        _socket_dir: socket_dir,
    })
}

fn print_applied(applied: &Applied, what: &str) -> Result<()> {
    if applied.ok {
        println!(
            "{what}: kept{}  revision {}  hash {}",
            applied
                .cell
                .as_ref()
                .map(|c| format!(" as {c}"))
                .unwrap_or_default(),
            applied.replay.revision,
            applied.replay.hash
        );
        Ok(())
    } else {
        let detail = applied
            .failure()
            .and_then(|f| f.error.as_ref())
            .map(|e| format!("{}: {}", e.kind, e.message))
            .unwrap_or_else(|| "the cell did not run".into());
        bail!("{what}: refused, nothing changed ({detail})")
    }
}

pub async fn chips(opened: &Opened, json: bool) -> Result<()> {
    let chips = opened.session.chips().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&chips)?);
        return Ok(());
    }
    for chip in chips {
        let kind = match chip.kind {
            ChipKind::Direct => "direct",
            ChipKind::Param => "slider",
            ChipKind::Seed => "seed",
        };
        let extra = match (&chip.slider, &chip.seed) {
            (Some(s), _) => format!("  {} = {} in [{}, {}]", s.name, s.value, s.min, s.max),
            (_, Some(seed)) => format!("  \"{seed}\""),
            _ => String::new(),
        };
        println!("{:<22} {:<7} {}{extra}", chip.id, kind, chip.label);
    }
    Ok(())
}

pub async fn fire_chip(opened: &mut Opened, id: &str) -> Result<()> {
    let applied = opened.session.fire_chip(id).await?;
    print_applied(&applied, id)
}

pub async fn set_params(opened: &mut Opened, assignments: &[String]) -> Result<()> {
    for assignment in assignments {
        let (name, value) = assignment
            .split_once('=')
            .with_context(|| format!("write a parameter as name=value, not `{assignment}`"))?;
        let value: f64 = value
            .trim()
            .parse()
            .with_context(|| format!("`{value}` is not a number"))?;
        let applied = opened.session.commit_param(name.trim(), value).await?;
        print_applied(&applied, &format!("{} = {value}", name.trim()))?;
    }
    Ok(())
}

fn print_turn(report: &TurnReport) {
    for event in &report.events {
        match event {
            AgentEvent::Evidence(e) => {
                let result = e.result.to_string();
                let shown: String = result.chars().take(160).collect();
                println!(
                    "  {} {}({}) -> {shown}{}",
                    e.id,
                    e.function,
                    e.args,
                    if result.len() > shown.len() {
                        "…"
                    } else {
                        ""
                    }
                );
            }
            AgentEvent::Claim(c) => {
                let badge = match c.status {
                    ClaimStatus::Proven => "proven",
                    ClaimStatus::Checked => "checked",
                    ClaimStatus::Unverified => "unverified",
                };
                let cites = if c.support.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", c.support.join(", "))
                };
                let note = c
                    .note
                    .as_ref()
                    .map(|n| format!("  [{n}]"))
                    .unwrap_or_default();
                println!("[{badge}] {}{cites}{note}", c.text);
            }
            AgentEvent::Cell {
                cell,
                caption,
                ok: true,
                ..
            } => {
                println!("cell {} kept: {caption}", cell.as_deref().unwrap_or("-"));
            }
            AgentEvent::Cell {
                caption,
                ok: false,
                error,
                ..
            } => {
                println!(
                    "cell refused: {caption} ({})",
                    error.as_deref().unwrap_or("did not run")
                );
            }
            _ => {}
        }
    }
    if !report.text.is_empty() {
        println!("{}", report.text);
    }
    match &report.end {
        TurnEnd::Done => {}
        TurnEnd::StepLimit => println!("(stopped at the step limit)"),
        TurnEnd::ModelError(m) => println!("(model error: {m})"),
    }
    if let Some(v) = report.verified.as_ref().filter(|v| !v.consistent) {
        println!(
            "warning: replaying the notebook from the top gave a different scene ({} cell(s) checked): \
             a cell does not do the same thing each time it runs. The scene shown is the replayed one.",
            v.checked
        );
    }
}

pub struct ChatArgs {
    pub message: Option<String>,
    pub plan: bool,
    pub model: Option<String>,
    pub json: bool,
    pub transcript: Option<PathBuf>,
}

pub async fn chat(opened: &mut Opened, args: ChatArgs) -> Result<()> {
    let home = config::home();
    let cfg = config::load(&home)?;
    let client = config::client(&home, &cfg, args.model.as_deref())?;
    let mut agent = Agent::new(
        client,
        AgentConfig {
            mode: if args.plan { Mode::Plan } else { Mode::Explore },
            transcript: args.transcript,
            ..AgentConfig::default()
        },
    );
    let mut failed = false;
    let mut one = |report: TurnReport, json: bool| -> Result<()> {
        if json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            print_turn(&report);
        }
        failed |= matches!(report.end, TurnEnd::ModelError(_));
        Ok(())
    };
    match args.message {
        Some(message) => one(agent.turn(&mut opened.session, &message).await?, args.json)?,
        None => {
            // One turn per line of standard input.
            for line in std::io::stdin().lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                one(agent.turn(&mut opened.session, &line).await?, args.json)?;
            }
        }
    }
    if failed {
        bail!("the model call failed");
    }
    Ok(())
}

pub struct EvalArgs {
    pub tasks: PathBuf,
    pub only: Vec<String>,
    pub model: Option<String>,
    pub results: Option<PathBuf>,
    pub project_python: Option<PathBuf>,
}

/// Run evaluation tasks and print a report; `Ok(false)` when any task failed.
pub async fn eval_run(engine: EngineConfig, args: EvalArgs) -> Result<bool> {
    use math_core::eval;

    let home = config::home();
    let cfg = config::load(&home)?;
    let client = config::client(&home, &cfg, args.model.as_deref())?;
    let mut dirs = eval::list_tasks(&args.tasks)?;
    if !args.only.is_empty() {
        for name in &args.only {
            eval::require_task(&args.tasks.join(name))?;
        }
        dirs.retain(|d| {
            d.file_name()
                .is_some_and(|n| args.only.iter().any(|o| n == o.as_str()))
        });
    }
    if dirs.is_empty() {
        bail!("no tasks under {}", args.tasks.display());
    }
    let results_dir = args
        .results
        .unwrap_or_else(|| PathBuf::from("fibration-math-eval-results"));
    // Work directories hold Unix sockets, whose paths must stay short.
    let work_root = tempfile::Builder::new().prefix("meval-").tempdir()?;
    let mut replay = ReplayOptions::default();
    if let Some(p) = args.project_python {
        replay.project_python = p;
    }
    let opts = eval::EvalOptions {
        engine,
        replay,
        work_root: work_root.path().to_path_buf(),
    };
    let mut results = Vec::new();
    for dir in dirs {
        let mut result = eval::run_task(&dir, client.clone(), &opts).await?;
        // Keep what the task produced next to the scorecard.
        let keep = results_dir.join(&result.task);
        std::fs::create_dir_all(&keep)?;
        for name in ["scene.py", "events.jsonl", "calls.jsonl"] {
            let from = result.work_dir.join(name);
            if from.is_file() {
                std::fs::copy(&from, keep.join(name))?;
            }
        }
        result.work_dir = keep;
        print!("{}", eval::report(&result));
        results.push(result);
    }
    let card = eval::write_scorecard(&results_dir, &results)?;
    let passed = results.iter().filter(|r| r.passed).count();
    println!(
        "{passed}/{} tasks passed with {}; scorecard: {}",
        results.len(),
        results[0].model,
        card.display()
    );
    Ok(passed == results.len())
}

/// Render the scene to `out` (format from its extension) with its sidecar next to it.
pub async fn export(opened: &Opened, out: &Path, dpi: Option<u32>) -> Result<()> {
    let format = match out.extension().and_then(|e| e.to_str()) {
        Some(e @ ("svg" | "pdf" | "png")) => e.to_string(),
        _ => bail!("the output file must end in .svg, .pdf or .png"),
    };
    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.canonicalize()?.join(
        out.file_name()
            .context("the output path has no file name")?,
    );
    let mut params = serde_json::json!({"format": format, "path": path});
    if let Some(dpi) = dpi {
        params["dpi"] = dpi.into();
    }
    let result = opened
        .session
        .engine_request("export.render", params)
        .await?;
    if result["status"] == "error" {
        bail!(
            "{}",
            result["error"]["detail"]
                .as_str()
                .unwrap_or("the export failed")
        );
    }
    println!("wrote {}", path.display());
    println!("sidecar {}", result["sidecar"].as_str().unwrap_or(""));
    let sampled: Vec<String> = result["objects"]
        .as_array()
        .map(|objects| {
            objects
                .iter()
                .filter(|o| o["status"] != "certified")
                .map(|o| {
                    format!(
                        "{} ({})",
                        o["id"].as_str().unwrap_or("?"),
                        o["reason"].as_str().unwrap_or("sampled")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if sampled.is_empty() {
        println!("every object is certified");
    } else {
        println!("not certified: {}", sampled.join("; "));
    }
    Ok(())
}
