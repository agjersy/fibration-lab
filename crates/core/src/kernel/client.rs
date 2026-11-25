//! `KernelClient`: request ids, a pending-response map, a notification channel, typed helpers
//! for every method, and reconnect (`run.attach` after respawn).

use super::transport::{KernelTransport, SpawnedKernel};
use fibration_protocol::kernel::*;
use fibration_protocol::rpc::{Incoming, Request, RequestId, RpcError};
use fibration_protocol::PROTOCOL_VERSION;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{broadcast, oneshot, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("{0}")]
    Rpc(#[from] RpcError),
    #[error("kernel I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("kernel is not connected")]
    Disconnected,
    #[error("kernel exited while a request was pending")]
    Exited,
    #[error(
        "kernel did not answer `{method}` within {seconds} s (connection lost or kernel stuck)"
    )]
    Timeout { method: String, seconds: u64 },
    #[error("invalid kernel response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

/// Default time allowed for one kernel request (PROTOCOL 9). Run output arrives as
/// notifications and is not bounded by this; long calls pass their own limit.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Time allowed for `kernel.hello` after spawning.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(60);

/// Calls that analyse the whole repository get more time.
const SLOW_CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Byte offsets already consumed for a run, so `run.attach` resumes where streaming stopped.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOffsets {
    pub stdout: u64,
    pub stderr: u64,
}

/// Pending requests keyed by id, tagged with the connection generation that sent them so a
/// reader task that ends for an old connection never fails requests of a newer one.
type Pending = HashMap<RequestId, (u64, oneshot::Sender<Result<Value, RpcError>>)>;

struct Inner {
    transport: Box<dyn KernelTransport>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<super::transport::KernelChild>>,
    pending: Mutex<Pending>,
    next_id: AtomicI64,
    /// Default per-call timeout in milliseconds.
    call_timeout_ms: std::sync::atomic::AtomicU64,
    /// Incremented on every (re)spawn.
    generation: AtomicI64,
    notifications: broadcast::Sender<KernelNotification>,
    /// Runs started through this client that have not reported `run.done`.
    active_runs: Mutex<HashMap<String, RunOffsets>>,
    hello: Mutex<Option<HelloResult>>,
}

#[derive(Clone)]
pub struct KernelClient {
    inner: Arc<Inner>,
}

impl KernelClient {
    /// Spawn the kernel through a transport and complete `kernel.hello`.
    pub async fn connect(
        transport: Box<dyn KernelTransport>,
        core_version: &str,
    ) -> Result<KernelClient, KernelError> {
        let (tx, _) = broadcast::channel(4096);
        let inner = Arc::new(Inner {
            transport,
            stdin: Mutex::new(None),
            child: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            call_timeout_ms: std::sync::atomic::AtomicU64::new(
                DEFAULT_CALL_TIMEOUT.as_millis() as u64
            ),
            generation: AtomicI64::new(0),
            notifications: tx,
            active_runs: Mutex::new(HashMap::new()),
            hello: Mutex::new(None),
        });
        let client = KernelClient { inner };
        client.spawn_and_hello(core_version).await?;
        Ok(client)
    }

    async fn spawn_and_hello(&self, core_version: &str) -> Result<HelloResult, KernelError> {
        let spawned = self
            .inner
            .transport
            .spawn()
            .await
            .map_err(|e| KernelError::Other(e.to_string()))?;
        let SpawnedKernel {
            child,
            stdin,
            stdout,
        } = spawned;
        *self.inner.stdin.lock().await = Some(stdin);
        *self.inner.child.lock().await = Some(child);
        let generation = (self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1) as u64;
        // The reader holds only a weak reference: when the last client handle is dropped (or
        // a connect is abandoned by a timeout), `Inner` is freed, the child is killed by
        // `kill_on_drop`, and this task ends at EOF instead of keeping the process alive.
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => match weak.upgrade() {
                        Some(inner) => inner.dispatch_line(&line).await,
                        None => return,
                    },
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("kernel stdout read error: {e}");
                        break;
                    }
                }
            }
            // EOF: fail every pending request that belongs to this connection.
            let Some(inner) = weak.upgrade() else { return };
            let mut pending = inner.pending.lock().await;
            let stale: Vec<RequestId> = pending
                .iter()
                .filter(|(_, (g, _))| *g == generation)
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale {
                if let Some((_, tx)) = pending.remove(&id) {
                    let _ = tx.send(Err(RpcError {
                        code: fibration_protocol::rpc::codes::INTERNAL_ERROR,
                        message: "kernel exited".into(),
                        data: None,
                    }));
                }
            }
            drop(pending);
            if inner.generation.load(Ordering::SeqCst) as u64 == generation {
                *inner.stdin.lock().await = None;
            }
        });
        let hello: HelloResult = self
            .call_with_timeout(
                "kernel.hello",
                HelloParams {
                    core_version: core_version.to_string(),
                    protocol_version: PROTOCOL_VERSION,
                },
                HELLO_TIMEOUT,
            )
            .await?;
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(KernelError::Other(format!(
                "kernel speaks protocol {} but core expects {PROTOCOL_VERSION}",
                hello.protocol_version
            )));
        }
        *self.inner.hello.lock().await = Some(hello.clone());
        Ok(hello)
    }

    /// The `kernel.hello` result of the current connection.
    pub async fn hello(&self) -> Option<HelloResult> {
        self.inner.hello.lock().await.clone()
    }

    pub fn describe(&self) -> String {
        self.inner.transport.describe()
    }

    /// Subscribe to kernel notifications. Each receiver sees every notification sent after
    /// subscribing (`run.stdout`, `run.stderr`, `run.artifact`, `run.done`).
    pub fn subscribe(&self) -> broadcast::Receiver<KernelNotification> {
        self.inner.notifications.subscribe()
    }

    /// Whether the kernel process is still attached (stdin open).
    pub async fn is_connected(&self) -> bool {
        self.inner.stdin.lock().await.is_some()
    }

    /// Send a raw JSON-RPC line (used by the `fibration kernel` passthrough).
    pub async fn send_raw(&self, line: &str) -> Result<(), KernelError> {
        let mut guard = self.inner.stdin.lock().await;
        let stdin = guard.as_mut().ok_or(KernelError::Disconnected)?;
        stdin.write_all(line.as_bytes()).await?;
        if !line.ends_with('\n') {
            stdin.write_all(b"\n").await?;
        }
        stdin.flush().await?;
        Ok(())
    }

    /// Change the default per-call timeout (tests, slow links).
    pub fn set_call_timeout(&self, timeout: Duration) {
        self.inner
            .call_timeout_ms
            .store(timeout.as_millis() as u64, Ordering::SeqCst);
    }

    /// The default per-call timeout.
    pub fn call_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.call_timeout_ms.load(Ordering::SeqCst))
    }

    /// Call a method and decode its result, with the default timeout.
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, KernelError> {
        self.call_with_timeout(method, params, self.call_timeout())
            .await
    }

    /// Call a method and decode its result, failing with [`KernelError::Timeout`] when no
    /// response arrives in time.
    pub async fn call_with_timeout<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
        timeout: Duration,
    ) -> Result<R, KernelError> {
        let v = self
            .call_value_with_timeout(method, serde_json::to_value(params)?, timeout)
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Call a method with raw JSON params and return the raw result (default timeout).
    pub async fn call_value(&self, method: &str, params: Value) -> Result<Value, KernelError> {
        self.call_value_with_timeout(method, params, self.call_timeout())
            .await
    }

    /// [`KernelClient::call_value`] with an explicit timeout.
    pub async fn call_value_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, KernelError> {
        let id = RequestId::Number(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        let generation = self.inner.generation.load(Ordering::SeqCst) as u64;
        self.inner
            .pending
            .lock()
            .await
            .insert(id.clone(), (generation, tx));
        let req = Request::new(id.clone(), method, params);
        let line = serde_json::to_string(&req)?;
        if let Err(e) = self.send_raw(&line).await {
            self.inner.pending.lock().await.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(KernelError::Rpc(e)),
            Ok(Err(_)) => Err(KernelError::Exited),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(KernelError::Timeout {
                    method: method.to_string(),
                    seconds: timeout.as_secs(),
                })
            }
        }
    }

    // ---- typed helpers ----

    pub async fn shutdown(&self) -> Result<(), KernelError> {
        let _: OkResult = self.call("kernel.shutdown", serde_json::json!({})).await?;
        *self.inner.stdin.lock().await = None;
        if let Some(mut child) = self.inner.child.lock().await.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        }
        Ok(())
    }

    pub async fn env_info(&self) -> Result<EnvInfo, KernelError> {
        self.call("env.info", serde_json::json!({})).await
    }

    pub async fn env_packages(&self) -> Result<PackagesResult, KernelError> {
        self.call("env.packages", serde_json::json!({})).await
    }

    pub async fn scripts_index(
        &self,
        globs: Option<Vec<String>>,
    ) -> Result<ScriptsIndexResult, KernelError> {
        self.call("scripts.index", ScriptsIndexParams { globs })
            .await
    }

    pub async fn lib_index(&self, package: Option<String>) -> Result<LibIndexResult, KernelError> {
        self.call("lib.index", LibIndexParams { package }).await
    }

    pub async fn run_start(&self, params: RunStartParams) -> Result<RunStartResult, KernelError> {
        let r: RunStartResult = self.call("run.start", params).await?;
        self.inner
            .active_runs
            .lock()
            .await
            .insert(r.run_id.clone(), RunOffsets::default());
        Ok(r)
    }

    pub async fn run_attach(
        &self,
        params: RunAttachParams,
    ) -> Result<RunAttachResult, KernelError> {
        let run_id = params.run_id.clone();
        let r: RunAttachResult = self.call("run.attach", params).await?;
        if !r.status.is_finished() {
            self.inner
                .active_runs
                .lock()
                .await
                .entry(run_id)
                .or_default();
        }
        Ok(r)
    }

    pub async fn run_status(&self, run_id: &str) -> Result<RunMeta, KernelError> {
        self.call(
            "run.status",
            RunIdParams {
                run_id: run_id.to_string(),
            },
        )
        .await
    }

    pub async fn run_cancel(&self, run_id: &str) -> Result<OkResult, KernelError> {
        self.call(
            "run.cancel",
            RunIdParams {
                run_id: run_id.to_string(),
            },
        )
        .await
    }

    pub async fn runs_list(&self, limit: Option<u64>) -> Result<RunsListResult, KernelError> {
        self.call("runs.list", RunsListParams { limit }).await
    }

    pub async fn exec_snippet(
        &self,
        code: &str,
        timeout_s: Option<f64>,
    ) -> Result<ExecSnippetResult, KernelError> {
        // The kernel enforces `timeout_s` (default 120 s); allow that plus a margin.
        let limit = Duration::from_secs_f64(timeout_s.unwrap_or(120.0).clamp(1.0, 86_400.0) + 30.0);
        self.call_with_timeout(
            "exec.snippet",
            ExecSnippetParams {
                code: code.to_string(),
                timeout_s,
            },
            limit.max(self.call_timeout()),
        )
        .await
    }

    pub async fn out_list(&self, params: OutListParams) -> Result<OutListResult, KernelError> {
        self.call("out.list", params).await
    }

    pub async fn out_read(&self, params: OutReadParams) -> Result<OutReadResult, KernelError> {
        self.call("out.read", params).await
    }

    pub async fn eq_extract(
        &self,
        params: EqExtractParams,
    ) -> Result<EqExtractResult, KernelError> {
        self.call("eq.extract", params).await
    }

    pub async fn eq_check(&self, params: EqCheckParams) -> Result<EqCheckResult, KernelError> {
        self.call_with_timeout("eq.check", params, self.slow_timeout())
            .await
    }

    pub async fn contract_check(
        &self,
        paths: Option<Vec<String>>,
    ) -> Result<DiagnosticsResult, KernelError> {
        self.call_with_timeout("contract.check", PathsParams { paths }, self.slow_timeout())
            .await
    }

    pub async fn contract_similar(
        &self,
        threshold: Option<f64>,
    ) -> Result<SimilarResult, KernelError> {
        self.call_with_timeout(
            "contract.similar",
            SimilarParams { threshold },
            self.slow_timeout(),
        )
        .await
    }

    pub async fn lint_run(
        &self,
        paths: Option<Vec<String>>,
    ) -> Result<DiagnosticsResult, KernelError> {
        self.call_with_timeout("lint.run", PathsParams { paths }, self.slow_timeout())
            .await
    }

    fn slow_timeout(&self) -> Duration {
        SLOW_CALL_TIMEOUT.max(self.call_timeout())
    }

    /// A guard that cancels `run_id` when dropped while still armed. Tool calls hold one for
    /// the run they started, so dropping the turn future (cancel) also stops the script.
    pub fn cancel_guard(&self, run_id: &str) -> RunCancelGuard {
        RunCancelGuard {
            kernel: Some(self.clone()),
            run_id: run_id.to_string(),
        }
    }

    /// Run ids started through this client that have not finished.
    pub async fn active_runs(&self) -> Vec<(String, RunOffsets)> {
        self.inner
            .active_runs
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    /// Respawn the kernel after a dropped connection or exit, then `run.attach` every active
    /// run from its recorded offsets so streaming resumes. Returns the new hello.
    pub async fn reconnect(&self, core_version: &str) -> Result<HelloResult, KernelError> {
        if let Some(mut child) = self.inner.child.lock().await.take() {
            child.kill_group();
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        *self.inner.stdin.lock().await = None;
        let pending: Vec<_> = self.inner.pending.lock().await.drain().collect();
        for (_, (_, tx)) in pending {
            let _ = tx.send(Err(RpcError {
                code: fibration_protocol::rpc::codes::INTERNAL_ERROR,
                message: "kernel reconnecting".into(),
                data: None,
            }));
        }
        let hello = self.spawn_and_hello(core_version).await?;
        let runs = self.active_runs().await;
        for (run_id, off) in runs {
            match self
                .run_attach(RunAttachParams {
                    run_id: run_id.clone(),
                    stdout_offset: Some(off.stdout),
                    stderr_offset: Some(off.stderr),
                })
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("run.attach {run_id} after reconnect failed: {e}");
                    self.inner.active_runs.lock().await.remove(&run_id);
                }
            }
        }
        Ok(hello)
    }
}

/// Cancels a run on drop unless [`RunCancelGuard::disarm`] was called (A10).
pub struct RunCancelGuard {
    kernel: Option<KernelClient>,
    run_id: String,
}

impl RunCancelGuard {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The run finished (or its owner wants it to continue): do not cancel on drop.
    pub fn disarm(mut self) {
        self.kernel = None;
    }
}

impl Drop for RunCancelGuard {
    fn drop(&mut self) {
        let Some(kernel) = self.kernel.take() else {
            return;
        };
        let run_id = std::mem::take(&mut self.run_id);
        // Drop runs outside any await point; the cancel request is sent from a task.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(e) = kernel.run_cancel(&run_id).await {
                    tracing::warn!("cancelling run {run_id} after its turn was dropped: {e}");
                }
            });
        }
    }
}

impl Inner {
    async fn dispatch_line(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let incoming: Incoming = match serde_json::from_str(line) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(
                    "unparseable kernel line ({e}): {}",
                    crate::util::truncate_bytes(line, 200)
                );
                return;
            }
        };
        match incoming {
            Incoming::Response(resp) => {
                let tx = self.pending.lock().await.remove(&resp.id);
                match tx {
                    Some((_, tx)) => {
                        let r = match (resp.result, resp.error) {
                            (_, Some(e)) => Err(e),
                            (Some(v), None) => Ok(v),
                            (None, None) => Ok(Value::Null),
                        };
                        let _ = tx.send(r);
                    }
                    None => tracing::warn!("kernel response for unknown id {}", resp.id),
                }
            }
            Incoming::Notification(n) => {
                let params = n.params.unwrap_or(Value::Null);
                match KernelNotification::parse(&n.method, params) {
                    Ok(kn) => {
                        self.track(&kn).await;
                        let _ = self.notifications.send(kn);
                    }
                    Err(e) => tracing::warn!("bad notification {}: {e}", n.method),
                }
            }
        }
    }

    async fn track(&self, n: &KernelNotification) {
        let mut runs = self.active_runs.lock().await;
        match n {
            KernelNotification::Stdout(o) => {
                if let Some(r) = runs.get_mut(&o.run_id) {
                    r.stdout = r.stdout.max(o.offset + o.chunk.len() as u64);
                }
            }
            KernelNotification::Stderr(o) => {
                if let Some(r) = runs.get_mut(&o.run_id) {
                    r.stderr = r.stderr.max(o.offset + o.chunk.len() as u64);
                }
            }
            KernelNotification::Done(d) => {
                runs.remove(&d.run_id);
            }
            _ => {}
        }
    }
}
