//! JSON-RPC 2.0 envelope types and error codes (PROTOCOL 1.1, 1.2).
//!
//! Ported from Personal Analyst `crates/protocol/src/rpc.rs`; the application error codes
//! and the typed `error.data` shapes are Present's.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// A request id: string or integer per the JSON-RPC spec.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl From<i64> for RequestId {
    fn from(v: i64) -> Self {
        RequestId::Number(v)
    }
}

impl From<String> for RequestId {
    fn from(v: String) -> Self {
        RequestId::String(v)
    }
}

impl From<&str> for RequestId {
    fn from(v: &str) -> Self {
        RequestId::String(v.to_string())
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Number(n) => write!(f, "{n}"),
            RequestId::String(s) => write!(f, "{s}"),
        }
    }
}

/// Core -> kernel request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Request {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Value) -> Self {
        Request {
            jsonrpc: "2.0".to_string(),
            id: id.into(),
            method: method.into(),
            params: Some(params),
        }
    }
}

/// Error object carried by a response. `data` always has `{detail: string}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// The `detail` field of `error.data`, if present.
    pub fn detail(&self) -> Option<&str> {
        self.data.as_ref()?.get("detail")?.as_str()
    }

    /// `error.data` as one of the typed shapes below, if it has that shape.
    pub fn data_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(self.data.clone()?).ok()
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rpc error {}: {}", self.code, self.message)?;
        if let Some(d) = self.detail() {
            write!(f, " ({d})")?;
        }
        Ok(())
    }
}

impl std::error::Error for RpcError {}

/// Error codes (PROTOCOL 1.2).
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Not found (slide, reference, layout, snapshot).
    pub const NOT_FOUND: i64 = 2000;
    /// Path escapes the project root.
    pub const PATH_OUTSIDE_ROOT: i64 = 2001;
    /// Schema validation failed; data is [`super::SchemaErrorData`].
    pub const SCHEMA_INVALID: i64 = 2002;
    /// Template unreadable or unsupported.
    pub const TEMPLATE_UNSUPPORTED: i64 = 2003;
    /// Render failed; data is [`super::RenderErrorData`].
    pub const RENDER_FAILED: i64 = 2004;
    /// Build script failed; data is [`super::BuildErrorData`].
    pub const BUILD_FAILED: i64 = 2005;
    pub const TIMEOUT: i64 = 2006;
    /// Sandbox requested but unavailable.
    pub const SANDBOX_UNAVAILABLE: i64 = 2007;
    /// Fidelity engine unavailable.
    pub const ENGINE_UNAVAILABLE: i64 = 2008;
    /// Unsupported reference format.
    pub const REF_FORMAT_UNSUPPORTED: i64 = 2009;
    /// Transform failed; data is [`super::TransformErrorData`].
    pub const TRANSFORM_FAILED: i64 = 2010;
}

/// `error.data` of every error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorData {
    pub detail: String,
}

/// `error.data` of code 2002.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaErrorData {
    pub detail: String,
    pub errors: Vec<SchemaErrorItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaErrorItem {
    /// JSON pointer into the validated document.
    pub pointer: String,
    pub message: String,
}

/// `error.data` of code 2004.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenderErrorData {
    pub detail: String,
    /// Present when the failure is attributable to one element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_id: Option<String>,
}

/// `error.data` of code 2005.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuildErrorData {
    pub detail: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// `error.data` of code 2010.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransformErrorData {
    pub detail: String,
    pub step_index: u32,
}

/// Any line the kernel writes on stdout: a response to a request, or a notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Incoming {
    Response(Response),
    Notification(RawNotification),
}

/// Kernel -> core response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Response {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Kernel -> core notification (no id), before its params are typed; see
/// [`crate::kernel::Notification`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RawNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_distinguishes_response_and_notification() {
        let r: Incoming =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        assert!(matches!(r, Incoming::Response(_)));
        let n: Incoming = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"log","params":{"level":"info","message":"x"}}"#,
        )
        .unwrap();
        assert!(matches!(n, Incoming::Notification(_)));
        let e: Incoming = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":"abc","error":{"code":2010,"message":"transform failed","data":{"detail":"no column Region","step_index":1}}}"#,
        )
        .unwrap();
        match e {
            Incoming::Response(r) => {
                let err = r.error.unwrap();
                assert_eq!(err.code, codes::TRANSFORM_FAILED);
                assert_eq!(err.detail(), Some("no column Region"));
                let data: TransformErrorData = err.data_as().unwrap();
                assert_eq!(data.step_index, 1);
                assert_eq!(
                    err.to_string(),
                    "rpc error 2010: transform failed (no column Region)"
                );
            }
            _ => panic!("expected response"),
        }
    }

    #[test]
    fn request_round_trip() {
        let req = Request::new(
            7,
            "kernel.hello",
            serde_json::json!({"core_version":"0.0.1","protocol_version":1}),
        );
        let s = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }
}
