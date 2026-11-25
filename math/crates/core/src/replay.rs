//! Notebook replay (ARCHITECTURE 6.3): reset the engine, run the cells from the top in a
//! fresh runner process, and read back the scene state.
//!
//! Runner contract (the Python side is `fibration_math.runner`):
//!
//! * started as `<project_python> -m fibration_math.runner --serve` with
//!   `FIBRATION_MATH_SOCKET` set to the attach socket;
//! * reads one JSON document per line, `{"cells": [{"id", "code"}, ...]}`, until stdin closes;
//! * executes each request's cells in order in one namespace kept across requests, stopping
//!   at the first error;
//! * prints one line of JSON per request:
//!   `{"results": [{"id", "status": "ok"|"error", "error"?: {type, message, traceback,
//!   call_error?}, "stdout": string}]}`.
//!
//! A replay is one request with every cell. The runner is kept afterwards so that an appended
//! cell can run alone (`KeptRunner`).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use math_protocol::{ReplayResult, RunnerCell, RunnerInput, RunnerOutput};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::notebook::Notebook;
use crate::router::RouterHandle;

/// Id given to the preamble when it has code to run.
pub const PREAMBLE_ID: &str = "_preamble";

#[derive(Clone, Debug)]
pub struct ReplayOptions {
    /// The interpreter that runs cells (the project venv). Defaults to
    /// `FIBRATION_MATH_PROJECT_PYTHON`, else the engine's development venv.
    pub project_python: PathBuf,
    /// Prepended to `PYTHONPATH` so the runner finds the SDK (development checkouts).
    pub sdk_path: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    /// Stop after this cell (inclusive).
    pub until: Option<String>,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        let project_python = std::env::var_os("FIBRATION_MATH_PROJECT_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(crate::engine::default_engine_python);
        let sdk = crate::dev_root().join("python/sdk");
        Self {
            project_python,
            sdk_path: sdk.exists().then_some(sdk),
            cwd: None,
            timeout: Duration::from_secs(120),
            until: None,
        }
    }
}

/// A cell runner that stays alive, holding the namespace the cells it has run built.
///
/// It is what lets an appended cell run alone (ARCHITECTURE 5.6): the cells before it are
/// already in the namespace. After a failed cell the namespace may hold part of that cell's
/// effects, so a runner is never used again after an error; the session starts a new one by
/// replaying from the top.
pub struct KeptRunner {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    stderr: Arc<Mutex<Vec<u8>>>,
    /// Scene isolation the runner got (`sandbox`, `limits`, ...), for reports.
    pub isolation: String,
}

impl KeptRunner {
    /// Start a runner with an empty namespace.
    pub async fn start(router: &RouterHandle, opts: &ReplayOptions) -> Result<Self> {
        let (mut cmd, isolation) = crate::sandbox::no_network_command_with_socket(
            opts.project_python.as_os_str(),
            ["-m", "fibration_math.runner", "--serve"],
            router.path(),
        );
        tracing::debug!(%isolation, "starting the cell runner");
        cmd.env("FIBRATION_MATH_SOCKET", router.path())
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(sdk) = &opts.sdk_path {
            let mut path = sdk.as_os_str().to_owned();
            if let Some(existing) = std::env::var_os("PYTHONPATH").filter(|p| !p.is_empty()) {
                path.push(":");
                path.push(existing);
            }
            cmd.env("PYTHONPATH", path);
        }
        if let Some(cwd) = &opts.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().with_context(|| {
            format!("starting the runner with {}", opts.project_python.display())
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("runner has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("runner has no stdout"))?;
        let mut err = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("runner has no stderr"))?;
        // Drain stderr so the runner never blocks on it; keep the tail for error reports.
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                if let Ok(mut kept) = sink.lock() {
                    kept.extend_from_slice(&buf[..n]);
                    let extra = kept.len().saturating_sub(STDERR_KEPT);
                    kept.drain(..extra);
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
            isolation: isolation.to_string(),
        })
    }

    /// Run cells in the kept namespace, in order, stopping at the first error.
    pub async fn run(&mut self, cells: Vec<RunnerCell>, timeout: Duration) -> Result<RunnerOutput> {
        let mut input = serde_json::to_vec(&RunnerInput { cells })?;
        input.push(b'\n');
        let exchange = async {
            self.stdin.write_all(&input).await?;
            self.stdin.flush().await?;
            let mut line = String::new();
            self.stdout.read_line(&mut line).await?;
            Ok::<_, std::io::Error>(line)
        };
        let line = match tokio::time::timeout(timeout, exchange).await {
            Ok(res) => res.context("running the cells")?,
            Err(_) => {
                let _ = self.child.start_kill();
                bail!("the cells did not finish within {timeout:?}; the runner was stopped")
            }
        };
        match serde_json::from_str(line.trim()) {
            Ok(output) => Ok(output),
            Err(_) => {
                let status = match self.child.try_wait() {
                    Ok(Some(status)) => status.to_string(),
                    _ => "still running".to_string(),
                };
                let _ = self.child.start_kill();
                // Give the drain task a moment to collect what the runner wrote last.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let tail = self
                    .stderr
                    .lock()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let tail: Vec<&str> = tail.lines().rev().take(20).collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                Err(anyhow!(
                    "the runner exited ({status}) without a result\n{}",
                    tail.join("\n")
                ))
            }
        }
    }
}

/// Bytes of the runner's stderr kept for error reports.
const STDERR_KEPT: usize = 64 * 1024;

/// Replay the notebook from the top in a fresh runner.
pub async fn replay(
    router: &RouterHandle,
    notebook: &Notebook,
    opts: &ReplayOptions,
) -> Result<ReplayResult> {
    Ok(replay_keeping(router, notebook, opts).await?.0)
}

/// Replay the notebook from the top, and keep the runner when every cell ran, so that a cell
/// appended next can run alone.
pub async fn replay_keeping(
    router: &RouterHandle,
    notebook: &Notebook,
    opts: &ReplayOptions,
) -> Result<(ReplayResult, Option<KeptRunner>)> {
    let mut cells: Vec<RunnerCell> = Vec::new();
    if notebook.preamble_has_code() {
        cells.push(RunnerCell {
            id: PREAMBLE_ID.into(),
            code: notebook.preamble.clone(),
        });
    }
    for cell in notebook.cells_until(opts.until.as_deref())? {
        cells.push(RunnerCell {
            id: cell.id.clone(),
            code: cell.code.clone(),
        });
    }

    let engine = router.router().engine();
    engine.reset().await.context("resetting the engine")?;
    router.router().begin_replay();
    let reads_before = router.router().state_reads();

    let mut runner = KeptRunner::start(router, opts).await?;
    let output = runner.run(cells, opts.timeout).await?;
    let result = finish(router, output.results, reads_before).await?;
    let keep = result.ok() && opts.until.is_none();
    Ok((result, keep.then_some(runner)))
}

/// Read the scene back after cells ran and put the result together.
pub(crate) async fn finish(
    router: &RouterHandle,
    results: Vec<math_protocol::CellResult>,
    reads_before: u64,
) -> Result<ReplayResult> {
    let state = router
        .router()
        .engine()
        .state_get()
        .await
        .context("reading the scene state")?;
    let revision = state.get("revision").and_then(Value::as_u64).unwrap_or(0);
    let hash = state
        .get("hash")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(ReplayResult {
        results,
        state,
        revision,
        hash,
        reads_state: router.router().state_reads() != reads_before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{engine_available, EngineClient, EngineConfig};
    use crate::router::Router;
    use math_protocol::CellStatus;

    /// Whether the runner module can be imported; the test skips until it can.
    fn runner_available(opts: &ReplayOptions) -> bool {
        if !opts.project_python.exists() {
            return false;
        }
        let mut cmd = std::process::Command::new(&opts.project_python);
        cmd.args(["-c", "import fibration_math.runner"]);
        if let Some(sdk) = &opts.sdk_path {
            cmd.env("PYTHONPATH", sdk);
        }
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    const SCENE: &str = "# fibration-math: scene api=1\n\
# %% [c1] Define\n\
from fibration_math import sym, plot, param\n\
x = sym.symbols(\"x\")\n\
param.declare(a=(1.0, -3, 3))\n\
f = sym.define(\"a*sin(x)\")\n\
plot.explicit(f, x=(-5, 5), id=\"curve\")\n\
\n\
# %% [c2] Set a\n\
param.set(a=2.0)\n\
\n\
# %% [c3] Fails\n\
raise ValueError(\"boom\")\n";

    async fn setup(dir: &std::path::Path) -> Option<(RouterHandle, ReplayOptions)> {
        let cfg = EngineConfig::from_env();
        let opts = ReplayOptions::default();
        if !engine_available(&cfg.python) || !runner_available(&opts) {
            eprintln!("skipping: engine python or fibration_math.runner not available");
            return None;
        }
        let engine = Arc::new(EngineClient::spawn(&cfg).await.unwrap());
        let router = Arc::new(Router::new(engine, Some(&dir.join("calls.jsonl"))).unwrap());
        Some((router.serve(&dir.join("sock/attach.sock")).unwrap(), opts))
    }

    #[tokio::test]
    async fn replay_is_deterministic_and_stops_at_errors() {
        let dir = tempfile::tempdir().unwrap();
        let Some((handle, mut opts)) = setup(dir.path()).await else {
            return;
        };
        let nb = Notebook::parse(SCENE).unwrap();

        opts.until = Some("c2".into());
        let first = replay(&handle, &nb, &opts).await.unwrap();
        assert!(first.ok(), "{:?}", first.results);
        assert_eq!(first.results.len(), 2);
        let again = replay(&handle, &nb, &opts).await.unwrap();
        assert_eq!(first.hash, again.hash);
        assert_eq!(first.revision, again.revision);

        opts.until = None;
        let full = replay(&handle, &nb, &opts).await.unwrap();
        assert!(!full.ok());
        let last = full.results.last().unwrap();
        assert_eq!(last.id, "c3");
        assert_eq!(last.status, CellStatus::Error);
        assert_eq!(last.error.as_ref().unwrap().kind, "ValueError");
        // The scene keeps the state of the last good cell.
        assert_eq!(full.hash, first.hash);
    }
}
