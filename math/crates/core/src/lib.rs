//! The Fibration Math core library (ARCHITECTURE 7): the notebook format, the engine client, the
//! call router behind the attach socket, and notebook replay.
//!
//! The core is the only path into the engine. Every call, whoever makes it, goes through the
//! router, which validates the framing, forwards the call, records it in the call log, and
//! broadcasts the resulting state diff.

pub mod agent;
pub mod chips;
pub mod config;
pub mod engine;
pub mod eval;
pub mod notebook;
pub mod replay;
pub mod router;
pub mod sandbox;
pub mod session;

pub use engine::{EngineClient, EngineConfig, EngineError};
pub use notebook::{Notebook, NotebookError};
pub use replay::{replay, ReplayOptions};
pub use router::{Router, RouterHandle};
pub use session::{Applied, CellEdit, SceneSession, SessionConfig};

use std::path::PathBuf;

/// The repository's `math/` directory, for development defaults and tests.
pub fn dev_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
