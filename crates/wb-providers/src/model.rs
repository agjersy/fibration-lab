// Ported from Personal Analyst `crates/protocol/src/model.rs` at commit f229e4d.
// Adapted: owned by this crate (no protocol crate dependency); adds document parts, the
// `Citation` event, an audit correlation tag, and the extended capability record
// (Present PROTOCOL 6.1).
//! Model provider layer types: chat requests, the internal event stream, capability record.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::config::ToolSupport;

/// Internal event stream produced by both wire adapters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelEvent {
    Text {
        delta: String,
    },
    /// Emitted once the call is complete.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// A citation attached to the text emitted just before it (Anthropic `citations_delta`
    /// with a `page_location`). `document_index` counts document parts in the request in
    /// order, from 0. Page numbers are passed through exactly as the wire gives them:
    /// `start_page` is 1-based and `end_page` is exclusive.
    Citation {
        document_index: u32,
        start_page: u32,
        end_page: u32,
        cited_text: String,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    Stop {
        reason: StopReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    End,
    ToolCalls,
    MaxTokens,
    Error,
}

/// Message roles in a chat request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A part of a message's content. Providers map these to their wire formats.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// Base64-encoded image.
    Image {
        media_type: String,
        data: String,
    },
    /// Base64-encoded document (PDF) in a user message. Sent as a native document block with
    /// citations enabled where the endpoint accepts them; otherwise replaced by a short text
    /// note, so callers should only send documents when the capability record says
    /// `documents: true` and fall back to extracted text themselves.
    Document {
        media_type: String,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// An assistant tool call (model-facing `name`).
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// The result for an earlier tool call, in a user message.
    ToolResult {
        tool_call_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentPart::Text { text: text.into() }],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: vec![ContentPart::Text { text: text.into() }],
        }
    }

    /// Concatenated text parts.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.content {
            if let ContentPart::Text { text } = p {
                out.push_str(text);
            }
        }
        out
    }
}

/// A tool definition as shown to the model. `name` is model-facing (see `names`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON schema object.
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChatRequest {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Opaque caller-chosen correlation id. Never sent to the endpoint; copied into the
    /// `AuditRecord` for this request so the caller can join it with its own context
    /// (project, conversation, turn, included references).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_tag: Option<String>,
}

/// Default `max_tokens` when a model has no `max_output_tokens` override. A smaller value
/// truncates file-creation tool calls (Analyst REVIEW B6).
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16384;

/// Baseline context window (tokens) below which a model is flagged degraded.
pub const BASELINE_CONTEXT_WINDOW: u32 = 131_072;

/// `degraded_reasons` codes.
pub const REASON_TOOLS: &str = "tools_not_native";
pub const REASON_CONTEXT: &str = "context_window_below_131072";
pub const REASON_VISION: &str = "no_vision";

/// Probed capability record, cached in `<home>/capabilities.json` keyed by `provider/model`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelCapabilities {
    pub context_window: u32,
    pub tools: ToolSupport,
    pub vision: bool,
    /// The endpoint accepts native document blocks.
    #[serde(default)]
    pub documents: bool,
    /// The endpoint accepts prompt-caching markers.
    #[serde(default)]
    pub caching: bool,
    /// Optional request fields the endpoint rejected (unknown-field downgrade), by wire name:
    /// `stream_options`, `cache_control`, `citations`, `document`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_fields: Vec<String>,
    /// Below the capability baseline: native tools, context window of at least
    /// `BASELINE_CONTEXT_WINDOW`, vision.
    #[serde(default)]
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded_reasons: Vec<String>,
    pub probed_at: String,
    /// `max_output_tokens` from the `[[models]]` override, when one is set. Never probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        let mut c = ModelCapabilities {
            context_window: 32768,
            tools: ToolSupport::Native,
            vision: false,
            documents: false,
            caching: false,
            unsupported_fields: Vec::new(),
            degraded: false,
            degraded_reasons: Vec::new(),
            probed_at: String::new(),
            max_output_tokens: None,
        };
        c.recompute_degraded();
        c
    }
}

impl ModelCapabilities {
    /// Recompute `degraded` and `degraded_reasons` against the baseline. Call after changing
    /// `tools`, `context_window`, or `vision`.
    pub fn recompute_degraded(&mut self) {
        let mut reasons = Vec::new();
        if self.tools != ToolSupport::Native {
            reasons.push(REASON_TOOLS.to_string());
        }
        if self.context_window < BASELINE_CONTEXT_WINDOW {
            reasons.push(REASON_CONTEXT.to_string());
        }
        if !self.vision {
            reasons.push(REASON_VISION.to_string());
        }
        self.degraded = !reasons.is_empty();
        self.degraded_reasons = reasons;
    }

    /// Whether `field` was recorded as rejected by the endpoint.
    pub fn is_unsupported(&self, field: &str) -> bool {
        self.unsupported_fields.iter().any(|f| f == field)
    }

    /// Record a rejected optional field and switch off the capability it carries.
    pub fn mark_unsupported(&mut self, field: &str) {
        if !self.is_unsupported(field) {
            self.unsupported_fields.push(field.to_string());
            self.unsupported_fields.sort();
        }
        match field {
            crate::wire::FIELD_CACHE_CONTROL => self.caching = false,
            crate::wire::FIELD_DOCUMENT => self.documents = false,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_event_round_trip() {
        let evs = vec![
            ModelEvent::Text { delta: "a".into() },
            ModelEvent::ToolCall {
                id: "1".into(),
                name: "fs_read".into(),
                arguments: json!({"path":"x"}),
            },
            ModelEvent::Citation {
                document_index: 0,
                start_page: 3,
                end_page: 4,
                cited_text: "q".into(),
            },
            ModelEvent::Usage {
                input_tokens: 3,
                output_tokens: 4,
            },
            ModelEvent::Stop {
                reason: StopReason::ToolCalls,
                message: None,
            },
        ];
        let s = serde_json::to_string(&evs).unwrap();
        assert!(s.contains("\"type\":\"tool_call\""));
        assert!(s.contains("\"type\":\"citation\""));
        assert!(s.contains("\"reason\":\"tool_calls\""));
        let back: Vec<ModelEvent> = serde_json::from_str(&s).unwrap();
        assert_eq!(back, evs);
    }

    #[test]
    fn chat_request_round_trip() {
        let req = ChatRequest {
            model: "m".into(),
            system: Some("sys".into()),
            messages: vec![
                Message::user_text("hi"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::ToolCall {
                        id: "c".into(),
                        name: "fs_read".into(),
                        arguments: json!({}),
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentPart::ToolResult {
                            tool_call_id: "c".into(),
                            content: "ok".into(),
                            is_error: false,
                        },
                        ContentPart::Image {
                            media_type: "image/png".into(),
                            data: "AAAA".into(),
                        },
                        ContentPart::Document {
                            media_type: "application/pdf".into(),
                            data: "JVBE".into(),
                            title: Some("Report".into()),
                        },
                    ],
                },
            ],
            tools: vec![ToolDef {
                name: "fs_read".into(),
                description: "d".into(),
                parameters: json!({"type":"object"}),
            }],
            max_tokens: Some(10),
            audit_tag: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: ChatRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn degraded_against_baseline() {
        let mut c = ModelCapabilities {
            context_window: 200_000,
            tools: ToolSupport::Native,
            vision: true,
            ..Default::default()
        };
        c.recompute_degraded();
        assert!(!c.degraded);
        assert!(c.degraded_reasons.is_empty());

        c.context_window = BASELINE_CONTEXT_WINDOW;
        c.recompute_degraded();
        assert!(!c.degraded, "exactly 131072 meets the baseline");

        c.context_window = BASELINE_CONTEXT_WINDOW - 1;
        c.tools = ToolSupport::None;
        c.vision = false;
        c.recompute_degraded();
        assert!(c.degraded);
        assert_eq!(
            c.degraded_reasons,
            vec![REASON_TOOLS, REASON_CONTEXT, REASON_VISION]
        );

        // The default record (32768, no vision) is below the baseline.
        let d = ModelCapabilities::default();
        assert!(d.degraded);
        assert_eq!(d.degraded_reasons, vec![REASON_CONTEXT, REASON_VISION]);
    }

    #[test]
    fn old_cache_records_still_parse() {
        let old =
            json!({"context_window": 65536, "tools": "native", "vision": false, "probed_at": "x"});
        let c: ModelCapabilities = serde_json::from_value(old).unwrap();
        assert!(!c.documents && !c.caching && c.unsupported_fields.is_empty());
    }

    #[test]
    fn mark_unsupported_switches_capability_off() {
        let mut c = ModelCapabilities {
            caching: true,
            documents: true,
            ..Default::default()
        };
        c.mark_unsupported("cache_control");
        c.mark_unsupported("cache_control");
        assert!(!c.caching && c.documents);
        assert_eq!(c.unsupported_fields, vec!["cache_control"]);
        c.mark_unsupported("document");
        assert!(!c.documents);
    }
}
