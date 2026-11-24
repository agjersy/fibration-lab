//! fibration-deck-protocol: serde types for every boundary fixed in docs/PROTOCOL.md.
//!
//! - [`deck`]: the deck document, schema version 1 (PROTOCOL 2).
//! - [`ids`]: validated slide, element, and reference ids, and the state id a build is
//!   stamped with (PROTOCOL 2.1, 2.3).
//! - [`cite`]: the citation grammar `ref:<ref_id>@<version>#<loc>` (PROTOCOL 2.3).
//! - [`template`]: template description and sidecar (PROTOCOL 3, 6.4).
//! - [`rpc`], [`kernel`]: JSON-RPC framing, error codes, kernel methods, diagnostics, and
//!   notifications (PROTOCOL 1).
//! - [`config`]: deck config and reference manifest (PROTOCOL 4).
//! - [`evals`]: evaluation task and scorecard types (PROTOCOL 12).
//! - [`schema`]: JSON Schema generation for `schemas/` (PROTOCOL 2).
//!
//! - [`ui`]: the deck views and events of the desktop IPC (IPC.md).
//!
//! Types that cross to the desktop UI derive `ts_rs::TS`; the test
//! `export_typescript_bindings` writes them to `apps/desktop/src/ipc/generated/deck/` (the
//! one renderer's deck types, next to the analysis types of `fibration-protocol`).

pub mod cite;
pub mod config;
pub mod deck;
pub mod evals;
pub mod ids;
pub mod kernel;
pub mod rpc;
pub mod schema;
pub mod template;
pub mod ui;

/// The kernel protocol version both sides must agree on (`kernel.hello`).
pub const PROTOCOL_VERSION: u32 = 1;

/// The deck document schema version (`Deck.schema_version`).
pub const SCHEMA_VERSION: u32 = 1;

/// Every tool result shown to the model is capped at this many bytes (PROTOCOL 6.3).
pub const TOOL_RESULT_CAP_BYTES: usize = 16 * 1024;
