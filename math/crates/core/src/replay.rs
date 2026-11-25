//! Notebook replay (ARCHITECTURE 6.3): reset the engine, run the cells from the top in a
//! fresh runner process, and read back the scene state.
//!
//! Runner contract (the Python side is `fibration_math.runner`):
//!
//! * started as `<project_python> -m fibration_math.runner` with `FIBRATION_MATH_SOCKET` set to the
//!   attach socket;
//! * reads ONE JSON document `{"cells": [{"id", "code"}, ...]}` from stdin until EOF;
//! * executes the cells in order in one interpreter, stopping at the first error;
//! * prints ONE line of JSON on stdout:
//!   `{"results": [{"id", "status": "ok"|"error", "error"?: {type, message, traceback,
//!   call_error?}, "stdout": string}]}` and exits.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use math_protocol::{ReplayResult, RunnerCell, RunnerInput, RunnerOutput};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

pub async fn replay(
    router: &RouterHandle,
    notebook: &Notebook,
    opts: &ReplayOptions,
) -> Result<ReplayResult> {
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

    let (mut cmd, isolation) = crate::sandbox::no_network_command_with_socket(
        opts.project_python.as_os_str(),
        ["-m", "fibration_math.runner"],
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
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting the runner with {}", opts.project_python.display()))?;

    let input = serde_json::to_vec(&RunnerInput { cells })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("runner has no stdin"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("runner has no stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("runner has no stderr"))?;

    let run = async {
        stdin.write_all(&input).await?;
        drop(stdin);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (r1, r2) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        r1?;
        r2?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((out, err, status))
    };
    let (out, err, status) = match tokio::time::timeout(opts.timeout, run).await {
        Ok(res) => res.context("running the cells")?,
        Err(_) => bail!(
            "the cells did not finish within {:?}; the runner was stopped",
            opts.timeout
        ),
    };

    let text = String::from_utf8_lossy(&out);
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let output: RunnerOutput = serde_json::from_str(line).map_err(|_| {
        let tail: String = String::from_utf8_lossy(&err)
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        anyhow!("the runner exited ({status}) without a result\n{tail}")
    })?;

    let state = engine
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
        results: output.results,
        state,
        revision,
        hash,
        reads_state: router.router().state_reads() != reads_before,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
