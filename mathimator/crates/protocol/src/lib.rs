//! Wire types shared by the Mathimator core, the CLI, and (later) the renderer.
//!
//! These mirror the engine's JSON-RPC contract (`mathimator_engine.server`, ARCHITECTURE 5.1
//! and 5.2) and the cell runner contract used by notebook replay (ARCHITECTURE 6.3).
//! Values the core does not interpret (`args`, `value`, `diff`, scene state) stay as
//! `serde_json::Value` so the engine stays the single authority on their shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The engine protocol version sent in `engine.hello`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Where a call came from (recorded in the call log and passed to the engine).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub kind: OriginKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OriginKind {
    #[default]
    Cell,
    Chip,
    Slider,
    Agent,
    Attach,
}

impl Origin {
    pub fn cell(id: impl Into<String>) -> Self {
        Self {
            kind: OriginKind::Cell,
            cell: Some(id.into()),
        }
    }

    pub fn of(kind: OriginKind) -> Self {
        Self { kind, cell: None }
    }
}

/// Parameters of the engine's `call` method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallRequest {
    #[serde(rename = "fn")]
    pub fn_name: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallStatus {
    Ok,
    Error,
}

/// A classified call failure (ARCHITECTURE 5.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallError {
    pub kind: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The result of one `call`: `status` says which of `value`/`error` is meaningful.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallResult {
    pub status: CallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<Value>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CallError>,
}

impl CallResult {
    pub fn is_ok(&self) -> bool {
        self.status == CallStatus::Ok
    }
}

/// A state change broadcast to subscribers after a call that changed the scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateDiff {
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub diff: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// One notebook cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub id: String,
    pub caption: String,
    pub code: String,
}

/// What the runner receives on stdin: the cells to execute, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerInput {
    pub cells: Vec<RunnerCell>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerCell {
    pub id: String,
    pub code: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellStatus {
    Ok,
    Error,
}

/// A Python exception raised by a cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellError {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub traceback: String,
    /// The line of the cell where the error was raised, when known (1-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Present when the exception was an engine call error surfaced by the SDK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_error: Option<CallError>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellResult {
    pub id: String,
    pub status: CellStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CellError>,
    #[serde(default)]
    pub stdout: String,
}

/// What the runner prints on stdout: one line of JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunnerOutput {
    pub results: Vec<CellResult>,
}

/// The outcome of replaying a notebook.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayResult {
    pub results: Vec<CellResult>,
    /// The engine's `state.get` after the replay.
    pub state: Value,
    pub revision: u64,
    pub hash: String,
}

impl ReplayResult {
    pub fn ok(&self) -> bool {
        self.results.iter().all(|r| r.status == CellStatus::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn call_result_reads_engine_shapes() {
        let ok: CallResult = serde_json::from_value(json!({
            "status": "ok", "value": {"$h": 1, "kind": "expr"}, "revision": 0, "ms": 0.2
        }))
        .unwrap();
        assert!(ok.is_ok());
        let err: CallResult = serde_json::from_value(json!({
            "status": "error", "error": {"kind": "parse", "detail": "x", "hint": "y"}, "revision": 3
        }))
        .unwrap();
        assert_eq!(err.error.unwrap().kind, "parse");
    }

    #[test]
    fn request_uses_fn_on_the_wire() {
        let req = CallRequest {
            fn_name: "diff".into(),
            args: json!({}),
            origin: Some(Origin::cell("c1")),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["fn"], "diff");
        assert_eq!(v["origin"], json!({"kind": "cell", "cell": "c1"}));
    }

    #[test]
    fn runner_output_reads() {
        let out: RunnerOutput = serde_json::from_value(json!({"results": [
            {"id": "c1", "status": "ok", "stdout": ""},
            {"id": "c2", "status": "error", "stdout": "", "error": {"type": "ValueError", "message": "m", "traceback": "t"}}
        ]}))
        .unwrap();
        assert_eq!(out.results[1].error.as_ref().unwrap().kind, "ValueError");
    }
}
