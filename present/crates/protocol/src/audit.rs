//! Audit log, `<home>/audit/YYYY-MM.jsonl`: one record per model request (PROTOCOL 10).
//!
//! A record never contains message content, keys, or headers; the type has no field that
//! could hold them.

use crate::cite::Cite;
use crate::config::Location;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct AuditRecord {
    /// RFC 3339 UTC.
    pub ts: String,
    pub project: String,
    pub conversation: String,
    pub turn: String,
    pub provider: String,
    pub location: Location,
    pub base_url_host: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Reference excerpts included in the request (tool results and document blocks).
    pub refs: Vec<AuditRef>,
    pub images: Vec<AuditImage>,
    pub status: AuditStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct AuditRef {
    pub cite: Cite,
    pub chars: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
pub struct AuditImage {
    pub source: AuditImageSource,
    pub id: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum AuditImageSource {
    Slide,
    Ref,
    Asset,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum AuditStatus {
    Ok,
    Error,
    Refused,
}
