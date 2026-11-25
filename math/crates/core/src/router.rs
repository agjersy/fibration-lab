//! The call router and the attach socket (ARCHITECTURE 6.2, 7).
//!
//! SDK clients (the cell runner, `python -m fibration-math attach`, any script) connect to a Unix
//! socket and speak newline-delimited JSON-RPC 2.0 with three methods: `call`, `state.get`,
//! and `describe_api`. The router forwards each request to the one engine (the engine client
//! serializes requests, so there is one call at a time per session), appends every `call` to
//! the JSONL call log, and broadcasts state diffs to subscribers.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use math_protocol::{CallRequest, CallResult, StateDiff};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::{EngineClient, EngineError};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// Longest request line the router accepts from a client.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

pub struct Router {
    engine: Arc<EngineClient>,
    log: Option<Mutex<std::fs::File>>,
    diffs: broadcast::Sender<StateDiff>,
    /// How many times a client has read the scene state.
    state_reads: AtomicU64,
    /// The scene revision after the latest call of each cell, since the last `begin_replay`.
    cell_revisions: Mutex<HashMap<String, u64>>,
}

impl Router {
    pub fn new(engine: Arc<EngineClient>, call_log: Option<&Path>) -> std::io::Result<Self> {
        let log = match call_log {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Some(Mutex::new(
                    OpenOptions::new().create(true).append(true).open(path)?,
                ))
            }
            None => None,
        };
        let (diffs, _) = broadcast::channel(256);
        Ok(Self {
            engine,
            log,
            diffs,
            state_reads: AtomicU64::new(0),
            cell_revisions: Mutex::new(HashMap::new()),
        })
    }

    pub fn engine(&self) -> &Arc<EngineClient> {
        &self.engine
    }

    /// Forget the per-cell revisions: a replay is starting from an empty scene.
    pub fn begin_replay(&self) {
        if let Ok(mut map) = self.cell_revisions.lock() {
            map.clear();
        }
    }

    /// The scene revision after the latest call made by `cell`, if it made any.
    pub fn revision_after(&self, cell: &str) -> Option<u64> {
        self.cell_revisions.lock().ok()?.get(cell).copied()
    }

    /// How many times a client has read the scene state through this router. A replay
    /// compares the count before and after to learn whether any cell looked at the state.
    pub fn state_reads(&self) -> u64 {
        self.state_reads.load(Ordering::Relaxed)
    }

    /// State diffs of every call that changed the scene, whoever made it.
    pub fn subscribe(&self) -> broadcast::Receiver<StateDiff> {
        self.diffs.subscribe()
    }

    /// Handle one request; `Err((code, detail))` becomes a JSON-RPC error.
    pub async fn handle(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        match method {
            "call" => self.call(params).await,
            "state.get" => {
                self.state_reads.fetch_add(1, Ordering::Relaxed);
                self.engine.state_get().await.map_err(rpc_error)
            }
            "describe_api" => {
                let with_state = params
                    .get("state")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.engine
                    .describe_api(with_state)
                    .await
                    .map_err(rpc_error)
            }
            other => Err((METHOD_NOT_FOUND, format!("unknown method {other:?}"))),
        }
    }

    async fn call(&self, params: Value) -> Result<Value, (i64, String)> {
        let req: CallRequest = serde_json::from_value(params.clone())
            .map_err(|e| (INVALID_PARAMS, format!("call: {e}")))?;
        let raw = self.engine.call_raw(params).await.map_err(rpc_error)?;
        let parsed: Option<CallResult> = serde_json::from_value(raw.clone()).ok();
        self.record(&req, parsed.as_ref());
        if let (Some(cell), Some(res)) = (
            req.origin.as_ref().and_then(|o| o.cell.as_ref()),
            parsed.as_ref(),
        ) {
            if let Ok(mut map) = self.cell_revisions.lock() {
                map.insert(cell.clone(), res.revision);
            }
        }
        if let Some(res) = parsed {
            if let Some(diff) = res
                .diff
                .filter(|d| d.as_object().is_some_and(|o| !o.is_empty()))
            {
                // No receivers is fine: nobody is watching yet.
                let _ = self.diffs.send(StateDiff {
                    revision: res.revision,
                    hash: res.hash,
                    diff,
                    origin: req.origin,
                });
            }
        }
        Ok(raw)
    }

    fn record(&self, req: &CallRequest, res: Option<&CallResult>) {
        let Some(log) = &self.log else { return };
        let mut entry = json!({
            "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "origin": req.origin,
            "fn": req.fn_name,
            "args": req.args,
        });
        match res {
            Some(r) => {
                entry["status"] = json!(r.status);
                entry["revision"] = json!(r.revision);
                entry["ms"] = json!(r.ms);
                if let Some(e) = &r.error {
                    entry["error_kind"] = json!(e.kind);
                }
            }
            None => entry["status"] = json!("unreadable"),
        }
        if let Ok(mut file) = log.lock() {
            if let Err(e) = writeln!(file, "{entry}") {
                tracing::warn!("call log write failed: {e}");
            }
        }
    }

    /// Serve the attach socket at `path` until the returned handle is dropped.
    pub fn serve(self: &Arc<Self>, path: &Path) -> std::io::Result<RouterHandle> {
        if let Some(parent) = path.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let router = Arc::clone(self);
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let router = Arc::clone(&router);
                        tokio::spawn(async move {
                            if let Err(e) = router.connection(stream).await {
                                tracing::debug!("attach connection ended: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!("attach socket accept failed: {e}");
                        break;
                    }
                }
            }
        });
        Ok(RouterHandle {
            path: path.to_path_buf(),
            router: Arc::clone(self),
            task,
        })
    }

    async fn connection(&self, stream: UnixStream) -> std::io::Result<()> {
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            if buf.len() > MAX_LINE {
                let reply = error_reply(Value::Null, INVALID_REQUEST, "request line too long");
                write.write_all(reply.as_bytes()).await?;
                return Ok(());
            }
            if buf.trim().is_empty() {
                continue;
            }
            let reply = self.reply(&buf).await;
            if let Some(reply) = reply {
                write.write_all(reply.as_bytes()).await?;
                write.flush().await?;
            }
        }
    }

    /// One request line to one response line (`None` for notifications).
    pub async fn reply(&self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return Some(error_reply(Value::Null, PARSE_ERROR, "not valid JSON")),
        };
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            return Some(error_reply(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "not a request",
            ));
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Some(error_reply(
                id.unwrap_or(Value::Null),
                INVALID_PARAMS,
                "params must be an object",
            ));
        }
        let result = self.handle(method, params).await;
        let id = id?;
        Some(match result {
            Ok(value) => format!("{}\n", json!({"jsonrpc": "2.0", "id": id, "result": value})),
            Err((code, detail)) => error_reply(id, code, &detail),
        })
    }
}

fn rpc_error(e: EngineError) -> (i64, String) {
    match e {
        EngineError::Rpc { code, detail } => (code, detail),
        other => (INTERNAL_ERROR, other.to_string()),
    }
}

fn error_reply(id: Value, code: i64, detail: &str) -> String {
    let message = match code {
        PARSE_ERROR => "parse error",
        INVALID_REQUEST => "invalid request",
        METHOD_NOT_FOUND => "method not found",
        INVALID_PARAMS => "invalid params",
        _ => "internal error",
    };
    format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message, "data": {"detail": detail}}})
    )
}

/// A running attach socket. Dropping it stops accepting and removes the socket file.
pub struct RouterHandle {
    path: PathBuf,
    router: Arc<Router>,
    task: JoinHandle<()>,
}

impl RouterHandle {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }
}

impl Drop for RouterHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{engine_available, EngineConfig};

    async fn router(log: &Path) -> Option<Arc<Router>> {
        let cfg = EngineConfig::from_env();
        if !engine_available(&cfg.python) {
            eprintln!("skipping: no engine python at {}", cfg.python.display());
            return None;
        }
        let engine = Arc::new(EngineClient::spawn(&cfg).await.unwrap());
        Some(Arc::new(Router::new(engine, Some(log)).unwrap()))
    }

    async fn rpc(stream: &mut BufReader<UnixStream>, line: &str) -> Value {
        stream.get_mut().write_all(line.as_bytes()).await.unwrap();
        stream.get_mut().write_all(b"\n").await.unwrap();
        let mut buf = String::new();
        stream.read_line(&mut buf).await.unwrap();
        serde_json::from_str(&buf).unwrap()
    }

    #[tokio::test]
    async fn socket_framing_log_and_diffs() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls.jsonl");
        let Some(router) = router(&log).await else {
            return;
        };
        let sock = dir.path().join("s/attach.sock");
        let handle = router.serve(&sock).unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(sock.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let mut diffs = router.subscribe();

        let mut s = BufReader::new(UnixStream::connect(&sock).await.unwrap());
        let r = rpc(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"call","params":{"fn":"param.declare","args":{"name":"a","value":1,"min":0,"max":2},"origin":{"kind":"cell","cell":"c1"}}}"#,
        )
        .await;
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["status"], "ok");
        let d = diffs.try_recv().unwrap();
        assert_eq!(d.revision, 1);
        assert_eq!(d.origin.unwrap().cell.as_deref(), Some("c1"));

        let r = rpc(
            &mut s,
            r#"{"jsonrpc":"2.0","id":2,"method":"call","params":{"fn":"nope"}}"#,
        )
        .await;
        assert_eq!(r["result"]["error"]["kind"], "unknown_fn");
        let r = rpc(
            &mut s,
            r#"{"jsonrpc":"2.0","id":3,"method":"reset","params":{}}"#,
        )
        .await;
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        let r = rpc(
            &mut s,
            r#"{"jsonrpc":"2.0","id":4,"method":"call","params":{"args":{}}}"#,
        )
        .await;
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        let r = rpc(&mut s, "not json").await;
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        let r = rpc(&mut s, r#"{"jsonrpc":"2.0","id":5,"method":"state.get"}"#).await;
        assert_eq!(r["result"]["revision"], 1);

        let lines: Vec<Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["fn"], "param.declare");
        assert_eq!(lines[0]["status"], "ok");
        assert_eq!(lines[1]["error_kind"], "unknown_fn");
        drop(handle);
        assert!(!sock.exists());
    }
}
