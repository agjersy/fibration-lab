//! Kernel client (PROTOCOL 1): transports, JSON-RPC framing, request/response matching,
//! notifications, typed helpers, and reconnect with `run.attach`.

pub mod client;
pub mod transport;

pub use client::{KernelClient, KernelError, RunCancelGuard, DEFAULT_CALL_TIMEOUT};
pub use transport::{
    resolve_tooling_python, resolve_tooling_python_in, KernelArgs, KernelTransport, LocalStdio,
    SpawnedKernel, SshStdio,
};

/// Wait for a run to finish, collecting its notifications. `rx` must have been subscribed
/// before the run started.
pub async fn wait_for_run(
    rx: &mut tokio::sync::broadcast::Receiver<analyst_protocol::kernel::KernelNotification>,
    run_id: &str,
    timeout: std::time::Duration,
) -> (
    Option<analyst_protocol::kernel::RunDone>,
    String,
    String,
    Vec<analyst_protocol::kernel::Artifact>,
) {
    use analyst_protocol::kernel::KernelNotification;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut artifacts = Vec::new();
    loop {
        let n = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(n)) => n,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            _ => return (None, stdout, stderr, artifacts),
        };
        if n.run_id() != Some(run_id) {
            continue;
        }
        match n {
            KernelNotification::Stdout(o) => stdout.push_str(&o.chunk),
            KernelNotification::Stderr(o) => stderr.push_str(&o.chunk),
            KernelNotification::Artifact(a) => artifacts.push(a.artifact),
            KernelNotification::Done(d) => return (Some(d), stdout, stderr, artifacts),
            KernelNotification::Other { .. } => {}
        }
    }
}
