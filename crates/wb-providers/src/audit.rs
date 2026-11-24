//! Audit hook (Present PROTOCOL 10): one record per model request, without content.
//! New in this crate.

use crate::config::Location;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditStatus {
    Ok,
    Error,
    /// The endpoint declined to answer (a refusal or content filter).
    Refused,
}

/// The provider-side part of a PROTOCOL 10 audit record. The caller adds project,
/// conversation, turn, and the lists of included references and images, joining on `tag`.
///
/// By construction this never holds message content, keys, or headers: `error` is a short
/// classification (`http_429`, `transport`, `stream`, `cancelled`, or a provider error type
/// such as `overloaded_error`), never the provider's free-text message, which could echo
/// request content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    /// RFC 3339 UTC time the request finished.
    pub ts: String,
    pub provider: String,
    pub location: Location,
    pub base_url_host: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub status: AuditStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `ChatRequest::audit_tag`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Receives one record per `ModelClient::stream` call made through a wire adapter, after the
/// response has ended (or failed, or been dropped by the consumer). A request retried by the
/// unknown-field downgrade is still one record. Called on a runtime worker thread; keep it
/// quick or hand off.
pub trait AuditSink: Send + Sync {
    fn record(&self, record: &AuditRecord);
}

/// Collects records in memory (tests and simple callers).
#[derive(Debug, Default)]
pub struct MemoryAuditSink {
    records: std::sync::Mutex<Vec<AuditRecord>>,
}

impl MemoryAuditSink {
    pub fn records(&self) -> Vec<AuditRecord> {
        self.records.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl AuditSink for MemoryAuditSink {
    fn record(&self, record: &AuditRecord) {
        if let Ok(mut r) = self.records.lock() {
            r.push(record.clone());
        }
    }
}
