//! Kernel client (PROTOCOL 1): transports, JSON-RPC framing, request/response matching,
//! notifications, typed helpers, and reconnect with `run.attach`.

pub mod client;
pub mod transport;

pub use client::{KernelClient, KernelError, RunCancelGuard, DEFAULT_CALL_TIMEOUT};
pub use transport::{
    resolve_tooling_python, resolve_tooling_python_in, KernelArgs, KernelTransport, LocalStdio,
    SpawnedKernel, SshStdio,
};
