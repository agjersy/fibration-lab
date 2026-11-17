//! `KernelClient`: the deck kernel over newline-delimited JSON-RPC on stdio (PROTOCOL 1.1).
//!
//! Adapted from Personal Analyst `crates/core/src/kernel/client.rs` and `transport.rs` at
//! commit f229e4d, reduced to the local stdio transport Present needs.

use present_protocol::kernel::{HelloParams, KernelHello, Method};
use present_protocol::rpc::RpcError;
use present_protocol::PROTOCOL_VERSION;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, Mutex};
use wb_sandbox::no_network::{self, NetIsolation};

#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("{} ({})", .0.message, detail_of(.0))]
    Rpc(RpcError),
    #[error("kernel I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("the kernel exited while a request was pending")]
    Exited,
    #[error("invalid kernel response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

fn detail_of(err: &RpcError) -> String {
    err.data
        .as_ref()
        .and_then(|d| d.get("detail"))
        .and_then(|d| d.as_str())
        .unwrap_or("no detail")
        .to_string()
}

impl KernelError {
    /// The JSON-RPC error code, when the kernel answered with an error.
    pub fn code(&self) -> Option<i64> {
        match self {
            KernelError::Rpc(e) => Some(e.code),
            _ => None,
        }
    }

    /// A field of `error.data` (stdout, stderr, slide_id, ...).
    pub fn data(&self, key: &str) -> Option<&Value> {
        match self {
            KernelError::Rpc(e) => e.data.as_ref().and_then(|d| d.get(key)),
            _ => None,
        }
    }
}

/// A notification from the kernel (`task.progress`, `log`).
#[derive(Debug, Clone)]
pub struct KernelNotification {
    pub method: String,
    pub params: Value,
}

/// How to launch the kernel for a project.
#[derive(Debug, Clone)]
pub struct KernelLaunch {
    pub python: PathBuf,
    pub project: PathBuf,
    pub home: PathBuf,
    pub engine: String,
    pub sandbox: String,
    /// Extra environment for the kernel process (font directories in tests, model paths).
    pub env: Vec<(String, String)>,
}

type Pending = HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>;

struct Inner {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: Arc<Mutex<Pending>>,
    next_id: AtomicI64,
    notifications: broadcast::Sender<KernelNotification>,
    hello: Value,
    network: NetIsolation,
}

#[derive(Clone)]
pub struct KernelClient {
    inner: Arc<Inner>,
}

/// Find the interpreter that runs the kernel (PROTOCOL 1.1).
pub fn resolve_runtime_python(home: &Path, configured: Option<&str>) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(env) = std::env::var("PRESENT_RUNTIME_PYTHON") {
        candidates.push(PathBuf::from(env));
    }
    if let Some(path) = configured.filter(|p| !p.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(home.join("runtime").join("bin").join("python3"));
    candidates.push(home.join("runtime").join(exe));
    // Development checkout: <repo>/python/kernel/.venv next to the workspace.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let venv = repo.join("python/kernel/.venv");
    candidates.push(venv.join("bin").join("python"));
    candidates.push(venv.join("Scripts").join("python.exe"));
    candidates.into_iter().find(|p| p.is_file())
}

/// `bwrap --bind / /` mounts the file system without device access, so the kernel could not
/// open `/dev/null` (which every `subprocess.DEVNULL` does). Bind `/` with its devices
/// instead; the network stays unshared. This changes nothing once the shared wrapper binds
/// devices itself, and nothing for the other isolation kinds.
fn keep_devices(mut wrapped: no_network::Wrapped) -> no_network::Wrapped {
    if wrapped.isolation == NetIsolation::Bubblewrap {
        let end = wrapped
            .argv
            .iter()
            .position(|a| a == "--")
            .unwrap_or(wrapped.argv.len());
        for i in 0..end.saturating_sub(2) {
            if wrapped.argv[i] == "--bind"
                && wrapped.argv[i + 1] == "/"
                && wrapped.argv[i + 2] == "/"
            {
                wrapped.argv[i] = "--dev-bind".into();
            }
        }
    }
    wrapped
}

impl KernelClient {
    /// Spawn the kernel and complete `kernel.hello`.
    ///
    /// The kernel runs with no network (`wb_sandbox::no_network`): it reads and writes the
    /// project on the file system and never needs to connect anywhere. The per-run sandboxes
    /// it starts itself (code mode, skill scripts) and the renderer it drives nest inside.
    /// Where the platform has no working isolation the kernel still starts, and
    /// [`KernelClient::network_isolation`] says so.
    pub async fn connect(launch: &KernelLaunch) -> Result<KernelClient, KernelError> {
        let argv: Vec<OsString> = [
            launch.python.as_os_str(),
            "-I".as_ref(),
            "-m".as_ref(),
            "present_kernel".as_ref(),
            "--project".as_ref(),
            launch.project.as_os_str(),
            "--home".as_ref(),
            launch.home.as_os_str(),
            "--engine".as_ref(),
            launch.engine.as_ref(),
            "--sandbox".as_ref(),
            launch.sandbox.as_ref(),
        ]
        .into_iter()
        .map(OsStr::to_os_string)
        .collect();
        let wrapped = keep_devices(no_network::wrap(argv));
        if let NetIsolation::None(why) = &wrapped.isolation {
            tracing::warn!(reason = %why, "the deck kernel is running without network isolation");
        }
        let mut cmd = Command::new(&wrapped.argv[0]);
        cmd.args(&wrapped.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in &launch.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(KernelError::Exited)?;
        let stdout = child.stdout.take().ok_or(KernelError::Exited)?;
        let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _) = broadcast::channel(1024);
        let reader_pending = pending.clone();
        let reader_tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    tracing::warn!("kernel sent a line that is not JSON");
                    continue;
                };
                if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                    let Some(sender) = reader_pending.lock().await.remove(&id) else {
                        continue;
                    };
                    let outcome = match msg.get("error") {
                        Some(err) => Err(serde_json::from_value::<RpcError>(err.clone())
                            .unwrap_or_else(|_| RpcError {
                                code: -32603,
                                message: "malformed error".into(),
                                data: None,
                            })),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = sender.send(outcome);
                } else if let Some(method) = msg.get("method").and_then(|v| v.as_str()) {
                    let _ = reader_tx.send(KernelNotification {
                        method: method.to_string(),
                        params: msg.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            // EOF: fail everything still pending.
            reader_pending.lock().await.clear();
        });
        let mut client = KernelClient {
            inner: Arc::new(Inner {
                stdin: Mutex::new(Some(stdin)),
                child: Mutex::new(Some(child)),
                pending,
                next_id: AtomicI64::new(1),
                notifications: tx,
                hello: Value::Null,
                network: wrapped.isolation,
            }),
        };
        let hello = client
            .call_value(
                KernelHello::NAME,
                serde_json::to_value(HelloParams {
                    core_version: env!("CARGO_PKG_VERSION").to_string(),
                    protocol_version: PROTOCOL_VERSION,
                })?,
            )
            .await?;
        // The only other handle is the reader task's clone of `pending`, so this succeeds.
        if let Some(inner) = Arc::get_mut(&mut client.inner) {
            inner.hello = hello;
        }
        Ok(client)
    }

    /// The raw `kernel.hello` result (engines, sandbox, embedding, packages).
    pub fn hello(&self) -> &Value {
        &self.inner.hello
    }

    /// How the kernel process is kept off the network (`label()` is `bwrap`,
    /// `sandbox-exec`, or `none`).
    pub fn network_isolation(&self) -> &NetIsolation {
        &self.inner.network
    }

    pub fn notifications(&self) -> broadcast::Receiver<KernelNotification> {
        self.inner.notifications.subscribe()
    }

    /// Call a method by wire name with JSON params; returns the id used with the result.
    pub async fn call_value(&self, method: &str, params: Value) -> Result<Value, KernelError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        let mut line = serde_json::to_string(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )?;
        line.push('\n');
        {
            let mut guard = self.inner.stdin.lock().await;
            let stdin = guard.as_mut().ok_or(KernelError::Exited)?;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }
        match rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(KernelError::Rpc(err)),
            Err(_) => Err(KernelError::Exited),
        }
    }

    /// Typed call for a method of PROTOCOL 1.3.
    pub async fn call<M: Method>(&self, params: &M::Params) -> Result<M::Result, KernelError> {
        let value = self
            .call_value(M::NAME, serde_json::to_value(params)?)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    /// `kernel.shutdown`, then wait for the process to exit.
    pub async fn shutdown(&self) {
        let _ = self.call_value("kernel.shutdown", json!({})).await;
        self.inner.stdin.lock().await.take();
        if let Some(mut child) = self.inner.child.lock().await.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_keeps_devices_and_only_touches_its_own_arguments() {
        let argv = |w: &no_network::Wrapped| -> Vec<String> {
            w.argv
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        // A kernel argument that looks like the wrapper's own is left alone.
        let command = ["python", "--bind", "/", "/"];
        let w = keep_devices(no_network::wrap_with(&NetIsolation::Bubblewrap, command));
        let a = argv(&w);
        let split = a.iter().position(|x| x == "--").unwrap();
        assert!(a[..split].contains(&"--unshare-net".to_string()));
        assert!(!a[..split].contains(&"--bind".to_string()));
        assert!(a[..split].contains(&"--dev-bind".to_string()));
        assert_eq!(a[split + 1..], command);

        let none = NetIsolation::None("why".into());
        let w = keep_devices(no_network::wrap_with(&none, command));
        assert_eq!(argv(&w), command);
        let w = keep_devices(no_network::wrap_with(&NetIsolation::SandboxExec, command));
        assert_eq!(argv(&w)[3..], command);
    }
}
