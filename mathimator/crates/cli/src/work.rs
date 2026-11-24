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
    let socket_dir = tempfile::Builder::new().prefix("mathimator-").tempdir()?;
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
            "{} does not run as it is; fix it first (`mathimator run` shows the error)",
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
