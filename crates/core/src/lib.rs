//! fibration-core: sessions, agent loop, policy, providers, kernel client, targets, sync,
//! checkpoints, tools, and the command table of the desktop IPC contract. The evaluation harness
//! is the `fibration-evals` crate. See docs/PROTOCOL.md and docs/ARCHITECTURE.md.

pub mod agent;
pub mod app;
pub mod checkpoint;
pub mod commands;
pub mod config;
pub mod egress;
pub mod init;
pub mod kernel;
pub mod migrate;
pub mod model_client;
pub mod service;
pub mod session;
pub mod skills;
pub mod target;
pub mod tools;
pub mod util;

pub use fibration_protocol as protocol;

/// Version string reported in `kernel.hello`.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
