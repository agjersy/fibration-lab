//! The engine client (ARCHITECTURE 5.1): spawns `python -m fibration_math_engine` and speaks
//! newline-delimited JSON-RPC 2.0 to it over stdio, one request at a time.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use math_protocol::{CallRequest, CallResult, PROTOCOL_VERSION};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

/// How long the engine may take to answer one request. Engine calls carry their own
/// budgets (at most 20 s); this is the backstop for a wedged process.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("could not start the engine with {python}: {source}")]
    Spawn {
        python: String,
        source: std::io::Error,
    },
    #[error("engine I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the engine exited")]
    Exited,
    #[error("the engine did not answer within {0:?}")]
    Timeout(Duration),
    #[error("the engine sent something that is not a JSON-RPC response: {0}")]
    Protocol(String),
    #[error("engine error {code}: {detail}")]
    Rpc { code: i64, detail: String },
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub python: PathBuf,
    pub memo_dir: Option<PathBuf>,
}

impl EngineConfig {
    /// `FIBRATION_MATH_ENGINE_PYTHON`, else the engine's development venv in this repository.
    pub fn from_env() -> Self {
        let python = std::env::var_os("FIBRATION_MATH_ENGINE_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(default_engine_python);
        Self {
            python,
            memo_dir: None,
        }
    }

    pub fn with_memo_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.memo_dir = Some(dir.into());
        self
    }
}

pub fn default_engine_python() -> PathBuf {
    crate::dev_root().join("python/engine/.venv/bin/python")
}

struct Io {
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    _child: Child,
}

pub struct EngineClient {
    io: Mutex<Io>,
    pub hello: Value,
    /// How the engine process is isolated: `bwrap`, `sandbox-exec`, or `none: <why>`.
    pub isolation: String,
}

impl EngineClient {
    pub async fn spawn(config: &EngineConfig) -> Result<Self, EngineError> {
        let mut args: Vec<std::ffi::OsString> = vec!["-m".into(), "fibration_math_engine".into()];
        if let Some(dir) = &config.memo_dir {
            args.push("--memo-dir".into());
            args.push(dir.into());
        }
        let (mut cmd, isolation) =
            crate::sandbox::no_network_command(config.python.as_os_str(), args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|source| EngineError::Spawn {
            python: config.python.display().to_string(),
            source,
        })?;
        let stdin = child.stdin.take().ok_or(EngineError::Exited)?;
        let stdout = child.stdout.take().ok_or(EngineError::Exited)?;
        let mut client = Self {
            io: Mutex::new(Io {
                stdin,
                lines: BufReader::new(stdout).lines(),
                next_id: 1,
                _child: child,
            }),
            hello: Value::Null,
            isolation,
        };
        client.hello = client
            .request(
                "engine.hello",
                json!({"core_version": env!("CARGO_PKG_VERSION"), "protocol_version": PROTOCOL_VERSION}),
            )
            .await?;
        Ok(client)
    }

    /// One JSON-RPC request; returns the `result` or the `error` as `EngineError::Rpc`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, EngineError> {
        let mut io = self.io.lock().await;
        let id = io.next_id;
        io.next_id += 1;
        let mut line = serde_json::to_string(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .map_err(|e| EngineError::Protocol(e.to_string()))?;
        line.push('\n');
        io.stdin.write_all(line.as_bytes()).await?;
        io.stdin.flush().await?;
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, io.lines.next_line())
            .await
            .map_err(|_| EngineError::Timeout(REQUEST_TIMEOUT))??
            .ok_or(EngineError::Exited)?;
        let msg: Value =
            serde_json::from_str(&reply).map_err(|_| EngineError::Protocol(reply.clone()))?;
        if msg.get("id") != Some(&json!(id)) {
            return Err(EngineError::Protocol(format!(
                "expected id {id}, got {reply}"
            )));
        }
        if let Some(err) = msg.get("error") {
            return Err(EngineError::Rpc {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                detail: err
                    .pointer("/data/detail")
                    .or_else(|| err.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
        msg.get("result")
            .cloned()
            .ok_or(EngineError::Protocol(reply))
    }

    /// The raw `call` result, exactly as the engine returned it.
    pub async fn call_raw(&self, params: Value) -> Result<Value, EngineError> {
        self.request("call", params).await
    }

    pub async fn call(&self, req: &CallRequest) -> Result<CallResult, EngineError> {
        let raw = self
            .call_raw(serde_json::to_value(req).map_err(|e| EngineError::Protocol(e.to_string()))?)
            .await?;
        serde_json::from_value(raw.clone()).map_err(|_| EngineError::Protocol(raw.to_string()))
    }

    pub async fn reset(&self) -> Result<Value, EngineError> {
        self.request("reset", json!({})).await
    }

    pub async fn state_get(&self) -> Result<Value, EngineError> {
        self.request("state.get", json!({})).await
    }

    pub async fn describe_api(&self, with_state: bool) -> Result<Value, EngineError> {
        self.request("describe_api", json!({"state": with_state}))
            .await
    }

    pub async fn restore(&self, revision: u64) -> Result<Value, EngineError> {
        self.request("state.restore", json!({"revision": revision}))
            .await
    }

    pub async fn shutdown(&self) -> Result<(), EngineError> {
        self.request("engine.shutdown", json!({})).await.map(|_| ())
    }
}

/// Whether the development engine environment exists (tests skip without it).
pub fn engine_available(python: &Path) -> bool {
    python.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Option<EngineConfig> {
        let cfg = EngineConfig::from_env();
        if engine_available(&cfg.python) {
            Some(cfg)
        } else {
            eprintln!("skipping: no engine python at {}", cfg.python.display());
            None
        }
    }

    #[tokio::test]
    async fn hello_call_and_errors() {
        let Some(cfg) = config() else { return };
        let engine = EngineClient::spawn(&cfg).await.unwrap();
        assert_eq!(engine.hello["protocol_version"], 1);
        let res = engine
            .call(&CallRequest {
                fn_name: "sym.define".into(),
                args: json!({"text": "x**2"}),
                origin: None,
            })
            .await
            .unwrap();
        assert!(res.is_ok());
        let bad = engine
            .call(&CallRequest {
                fn_name: "nope".into(),
                args: json!({}),
                origin: None,
            })
            .await
            .unwrap();
        assert_eq!(bad.error.unwrap().kind, "unknown_fn");
        let api = engine.describe_api(false).await.unwrap();
        assert!(api["operations"].as_array().unwrap().len() > 30);
        let err = engine
            .request("no.such.method", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Rpc { code: -32601, .. }));
        engine.shutdown().await.unwrap();
    }
}
