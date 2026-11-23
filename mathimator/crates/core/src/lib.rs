//! The Mathimator core library (ARCHITECTURE 7): the notebook format, the engine client, the
//! call router behind the attach socket, and notebook replay.
//!
//! The core is the only path into the engine. Every call, whoever makes it, goes through the
//! router, which validates the framing, forwards the call, records it in the call log, and
//! broadcasts the resulting state diff.

pub mod engine;
pub mod notebook;
pub mod replay;
pub mod router;
pub mod sandbox;

pub use engine::{EngineClient, EngineConfig, EngineError};
pub use notebook::{Notebook, NotebookError};
pub use replay::{replay, ReplayOptions};
pub use router::{Router, RouterHandle};

use std::path::PathBuf;

/// The repository's `mathimator/` directory, for development defaults and tests.
pub fn dev_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
