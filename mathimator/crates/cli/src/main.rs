//! `mathimator`: the headless command line (BUILD-SPEC WP-0.10, first commands).
//!
//! * `mathimator run <scene.py> [--until <cell>] [--json]` replays a scene notebook from the
//!   top and prints each cell's status and the final scene hash.
//! * `mathimator api` prints the engine's operation manifest.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use math_core::{replay, EngineClient, EngineConfig, Notebook, ReplayOptions, Router};
use math_protocol::CellStatus;

#[derive(Parser)]
#[command(
    name = "mathimator",
    version,
    about = "Mathimator: replayable, verified math scenes"
)]
struct Cli {
    /// Interpreter for the engine (default: MATHIMATOR_ENGINE_PYTHON, else the dev venv).
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
        /// Interpreter that runs the cells (default: MATHIMATOR_PROJECT_PYTHON, else the dev venv).
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
            let tmp = tempfile::Builder::new().prefix("mathimator-").tempdir()?;
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
        let cli =
            Cli::try_parse_from(["mathimator", "run", "s.py", "--until", "c2", "--json"]).unwrap();
        match cli.command {
            Command::Run { until, json, .. } => {
                assert_eq!(until.as_deref(), Some("c2"));
                assert!(json);
            }
            Command::Api => panic!("expected run"),
        }
        assert!(Cli::try_parse_from(["mathimator", "api"]).is_ok());
    }
}
