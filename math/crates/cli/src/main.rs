//! `fibration-math`: the headless command line (BUILD-SPEC WP-0.10, first commands).
//!
//! * `fibration-math run <scene.py> [--until <cell>] [--json]` replays a scene notebook from the
//!   top and prints each cell's status and the final scene hash.
//! * `fibration-math api` prints the engine's operation manifest.
//! * `fibration-math chat <scene.py> [-m <message>]` runs agent turns on a scene (one per line of
//!   standard input without `-m`), using the model configured in `<home>/config.toml`.
//! * `fibration-math export <scene.py> <out.svg>` renders a figure with its sidecar.
//! * `fibration-math eval --model <provider>/<id>` runs the evaluation tasks and writes a scorecard.
//! * `fibration-math chips <scene.py>` lists what can be done next; `chip <scene.py> <id>` fires a
//!   direct chip; `set <scene.py> a=2.5` commits a parameter. Each becomes a cell.

mod work;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use math_core::{replay, EngineClient, EngineConfig, Notebook, ReplayOptions, Router};
use math_protocol::CellStatus;

#[derive(Parser)]
#[command(
    name = "fibration-math",
    version,
    about = "Fibration Math: replayable, verified math scenes"
)]
struct Cli {
    /// Interpreter for the engine (default: FIBRATION_MATH_ENGINE_PYTHON, else the dev venv).
    #[arg(long, global = true)]
    engine_python: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Replay a scene notebook from the top.
    Run {
        scene: PathBuf,
        /// Stop after this cell.
        #[arg(long)]
        until: Option<String>,
        /// Print the full result (cell results and scene state) as JSON.
        #[arg(long)]
        json: bool,
        /// Interpreter that runs the cells (default: FIBRATION_MATH_PROJECT_PYTHON, else the dev venv).
        #[arg(long)]
        project_python: Option<PathBuf>,
        /// Keep memoized engine results here (default: <scene dir>/.cache/memo).
        #[arg(long)]
        memo_dir: Option<PathBuf>,
        /// Append every engine call to this JSONL file.
        #[arg(long)]
        call_log: Option<PathBuf>,
        /// Seconds the cells may run in total.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Print the engine's operation manifest.
    Api,
    /// Run agent turns on a scene.
    Chat {
        scene: PathBuf,
        /// The message for a single turn (default: one turn per line of standard input).
        #[arg(short, long)]
        message: Option<String>,
        /// Plan mode: the agent can check facts but not change the scene.
        #[arg(long)]
        plan: bool,
        /// `<provider>/<model id>` (default: default_model in <home>/config.toml).
        #[arg(long)]
        model: Option<String>,
        /// Print each turn's full report as one JSON line.
        #[arg(long)]
        json: bool,
        /// Append every event to this JSONL file.
        #[arg(long)]
        transcript: Option<PathBuf>,
        #[arg(long)]
        project_python: Option<PathBuf>,
    },
    /// Run evaluation tasks (`<tasks>/<name>/task.toml`) and write a scorecard.
    Eval {
        /// The directory holding the task directories.
        #[arg(long, default_value = "math/evals/tasks")]
        tasks: PathBuf,
        /// Run only these tasks (may be repeated).
        #[arg(long = "task")]
        only: Vec<String>,
        /// `<provider>/<model id>` (default: default_model in <home>/config.toml).
        #[arg(long)]
        model: Option<String>,
        /// Where the scorecard and each task's final scene and events are written.
        #[arg(long)]
        results: Option<PathBuf>,
        #[arg(long)]
        project_python: Option<PathBuf>,
    },
    /// Render a scene to a figure (.svg, .pdf or .png) with a sidecar recording what it shows.
    Export {
        scene: PathBuf,
        out: PathBuf,
        /// Dots per inch, for .png only.
        #[arg(long)]
        dpi: Option<u32>,
    },
    /// List the chips for a scene: what can be done next, with no model involved.
    Chips {
        scene: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Fire a direct chip; it becomes a cell.
    Chip { scene: PathBuf, id: String },
    /// Commit parameter values (`a=2.5`), as a slider release does; each becomes a cell.
    Set {
        scene: PathBuf,
        #[arg(required = true)]
        assignments: Vec<String>,
    },
}

fn engine_config(python: Option<PathBuf>) -> EngineConfig {
    let mut cfg = EngineConfig::from_env();
    if let Some(p) = python {
        cfg.python = p;
    }
    cfg
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Api => {
            let engine = EngineClient::spawn(&engine_config(cli.engine_python)).await?;
            let api = engine.describe_api(false).await?;
            println!("{}", serde_json::to_string_pretty(&api)?);
            engine.shutdown().await.ok();
            Ok(())
        }
        Command::Chat {
            scene,
            message,
            plan,
            model,
            json,
            transcript,
            project_python,
        } => {
            let mut opened =
                work::open(&scene, engine_config(cli.engine_python), project_python).await?;
            let args = work::ChatArgs {
                message,
                plan,
                model,
                json,
                transcript,
            };
            let result = work::chat(&mut opened, args).await;
            opened.session.close().await.ok();
            result
        }
        Command::Eval {
            tasks,
            only,
            model,
            results,
            project_python,
        } => {
            let args = work::EvalArgs {
                tasks,
                only,
                model,
                results,
                project_python,
            };
            if !work::eval_run(engine_config(cli.engine_python), args).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Export { scene, out, dpi } => {
            let opened = work::open(&scene, engine_config(cli.engine_python), None).await?;
            let result = work::export(&opened, &out, dpi).await;
            opened.session.close().await.ok();
            result
        }
        Command::Chips { scene, json } => {
            let opened = work::open(&scene, engine_config(cli.engine_python), None).await?;
            let result = work::chips(&opened, json).await;
            opened.session.close().await.ok();
            result
        }
        Command::Chip { scene, id } => {
            let mut opened = work::open(&scene, engine_config(cli.engine_python), None).await?;
            let result = work::fire_chip(&mut opened, &id).await;
            opened.session.close().await.ok();
            result
        }
        Command::Set { scene, assignments } => {
            let mut opened = work::open(&scene, engine_config(cli.engine_python), None).await?;
            let result = work::set_params(&mut opened, &assignments).await;
            opened.session.close().await.ok();
            result
        }
        Command::Run {
            scene,
            until,
            json,
            project_python,
            memo_dir,
            call_log,
            timeout,
        } => {
            let text = std::fs::read_to_string(&scene)
                .with_context(|| format!("reading {}", scene.display()))?;
            let notebook =
                Notebook::parse(&text).with_context(|| format!("parsing {}", scene.display()))?;
            let scene_dir = scene
                .canonicalize()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."));
            let memo = memo_dir.unwrap_or_else(|| scene_dir.join(".cache/memo"));
            let cfg = engine_config(cli.engine_python).with_memo_dir(memo);
            let engine = Arc::new(EngineClient::spawn(&cfg).await?);
            if engine.isolation.starts_with("none") {
                // As Analyst badges an unsandboxed kernel: say so, never run quietly unisolated.
                eprintln!(
                    "warning: network not isolated for the engine and cells ({})",
                    engine.isolation
                );
            }
            let tmp = tempfile::Builder::new()
                .prefix("fibration-math-")
                .tempdir()?;
            let router = Arc::new(Router::new(Arc::clone(&engine), call_log.as_deref())?);
            let handle = router.serve(&tmp.path().join("sock/attach.sock"))?;

            let mut opts = ReplayOptions {
                until,
                timeout: Duration::from_secs(timeout),
                ..ReplayOptions::default()
            };
            if let Some(p) = project_python {
                opts.project_python = p;
            }
            let result = replay(&handle, &notebook, &opts).await?;
            drop(handle);
            engine.shutdown().await.ok();

            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                for r in &result.results {
                    let caption = notebook
                        .cell(&r.id)
                        .map(|c| c.caption.as_str())
                        .unwrap_or("");
                    match (&r.status, &r.error) {
                        (CellStatus::Ok, _) => println!("{:<10} ok     {caption}", r.id),
                        (CellStatus::Error, Some(e)) => {
                            // The SDK's exception message already carries the engine's hint.
                            println!(
                                "{:<10} error  {caption}\n           {}: {}",
                                r.id, e.kind, e.message
                            );
                        }
                        (CellStatus::Error, None) => println!("{:<10} error  {caption}", r.id),
                    }
                    // What the cell printed, indented under its status line.
                    for line in r.stdout.lines() {
                        println!("           | {line}");
                    }
                }
                println!("revision {}  hash {}", result.revision, result.hash);
            }
            if !result.ok() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arguments() {
        let cli = Cli::try_parse_from(["fibration-math", "run", "s.py", "--until", "c2", "--json"])
            .unwrap();
        match cli.command {
            Command::Run { until, json, .. } => {
                assert_eq!(until.as_deref(), Some("c2"));
                assert!(json);
            }
            _ => panic!("expected run"),
        }
        assert!(Cli::try_parse_from(["fibration-math", "api"]).is_ok());
        assert!(
            Cli::try_parse_from(["fibration-math", "chat", "s.py", "-m", "hi", "--plan"]).is_ok()
        );
        assert!(Cli::try_parse_from(["fibration-math", "set", "s.py"]).is_err());
        assert!(Cli::try_parse_from(["fibration-math", "chip", "s.py", "view:fit"]).is_ok());
    }
}
