//! Personal Present core: projects, the deck store, snapshots, the reference index, the audit
//! log, and the kernel client. The agent loop builds on these (BUILD-SPEC W10, W12).

pub mod agent;
pub mod audit;
pub mod config;
pub mod deck_store;
pub mod index;
pub mod kernel;
pub mod project;
pub mod refs;
pub mod snapshots;
pub mod util;
pub mod workspace;
