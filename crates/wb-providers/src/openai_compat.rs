// Ported from Personal Analyst `crates/core/src/providers/openai_compat.rs` at commit f229e4d.
// Adapted (Present PROTOCOL 6.1): configurable auth and extra headers, the `stream_options`
// downgrade, document parts as text notes, and the audit hook (the request pump now lives
// in `wire.rs`).
//! OpenAI-compatible chat completions adapter.

use crate::config::AuthKind;
use crate::model::{ChatRequest, ContentPart, ModelCapabilities, ModelEvent, Role, StopReason};
use crate::sse::SseEvent;
use crate::wire::{
    self, Decoder, Endpoint, EndpointConfig, Features, OptionalField, WireSpec,
    FIELD_STREAM_OPTIONS,
};
use crate::ModelClient;
use futures::stream::BoxStream;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

const FIXED_HEADERS: &[(&str, &str)] = &[("content-type", "application/json")];

/// Build the JSON request body (with `stream_options`).
pub fn build_body(req: &ChatRequest) -> Value {
    build_body_with(req, true)
}

fn build_for(req: &ChatRequest, features: &Features) -> Value {
    build_body_with(req, features.allows(FIELD_STREAM_OPTIONS))
}

static OPTIONAL: &[OptionalField] = &[OptionalField {
    name: FIELD_STREAM_OPTIONS,
    sent: |b| b.get("stream_options").is_some(),
    named: |t| t.contains("stream_options"),
}];

/// Build the JSON request body; `usage_in_stream` adds `stream_options: {include_usage: true}`.
pub fn build_body_with(req: &ChatRequest, usage_in_stream: bool) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = &req.system {
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
    }
    for m in &req.messages {
        match m.role {
            Role::Assistant => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for p in &m.content {
                    match p {
                        ContentPart::Text { text: t } => text.push_str(t),
                        ContentPart::ToolCall {
                            id,
                            name,
                            arguments,
                        } => tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into())
                            }
                        })),
                        _ => {}
                    }
                }
                let mut msg = json!({"role": "assistant"});
                msg["content"] = if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                };
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(msg);
            }
            Role::User => {
                // Tool results become `role: tool` messages, in order, before the user content.
                let mut parts: Vec<Value> = Vec::new();
                for p in &m.content {
                    match p {
                        ContentPart::ToolResult {
                            tool_call_id,
                            content,
                            ..
                        } => messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": content
                        })),
                        ContentPart::Text { text } => {
                            parts.push(json!({"type":"text","text":text}))
                        }
                        ContentPart::Image { media_type, data } => parts.push(json!({
                            "type": "image_url",
                            "image_url": {"url": format!("data:{media_type};base64,{data}")}
                        })),
                        // This wire format has no document block.
                        ContentPart::Document { title, .. } => parts.push(json!({
                            "type": "text",
                            "text": crate::anthropic::document_note(title.as_deref())
                        })),
                        ContentPart::ToolCall { .. } => {}
                    }
                }
                if !parts.is_empty() {
                    let all_text = parts.iter().all(|p| p["type"] == "text");
                    let content = if all_text {
                        Value::String(
                            parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join(""),
                        )
                    } else {
                        Value::Array(parts)
                    };
                    messages.push(json!({"role": "user", "content": content}));
                }
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
    });
    if usage_in_stream {
        body["stream_options"] = json!({"include_usage": true});
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.parameters}})
                })
                .collect(),
        );
    }
    if let Some(m) = req.max_tokens {
        body["max_tokens"] = json!(m);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    body
}

#[derive(Debug, Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

/// Incremental state for one streamed response.
#[derive(Debug, Default)]
pub struct StreamState {
    calls: BTreeMap<u64, PendingCall>,
    finish_reason: Option<String>,
    usage: Option<(u64, u64)>,
    error_type: Option<String>,
    done: bool,
}

impl StreamState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one `data:` payload; returns the events it produces.
    pub fn feed(&mut self, data: &str) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        if data.trim() == "[DONE]" {
            out.extend(self.finish());
            return out;
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                self.done = true;
                out.push(ModelEvent::Stop {
                    reason: StopReason::Error,
                    message: Some(format!("malformed SSE JSON: {e}")),
                });
                return out;
            }
        };
        if let Some(err) = v.get("error") {
            self.done = true;
            self.error_type = ["type", "code"]
                .iter()
                .find_map(|k| err.get(k).and_then(Value::as_str))
                .map(str::to_string);
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("provider error")
                .to_string();
            out.push(ModelEvent::Stop {
                reason: StopReason::Error,
                message: Some(msg),
            });
            return out;
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            let i = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
            let o = u
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            self.usage = Some((i, o));
        }
        if let Some(choice) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        {
            if let Some(delta) = choice.get("delta") {
                if let Some(t) = delta.get("content").and_then(Value::as_str) {
                    if !t.is_empty() {
                        out.push(ModelEvent::Text {
                            delta: t.to_string(),
                        });
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for (i, c) in calls.iter().enumerate() {
                        let idx = c.get("index").and_then(Value::as_u64).unwrap_or(i as u64);
                        let entry = self.calls.entry(idx).or_default();
                        if let Some(id) = c.get("id").and_then(Value::as_str) {
                            entry.id = id.to_string();
                        }
                        if let Some(f) = c.get("function") {
                            if let Some(n) = f.get("name").and_then(Value::as_str) {
                                entry.name.push_str(n);
                            }
                            if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                                entry.arguments.push_str(a);
                            }
                        }
                    }
                }
            }
            if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(fr.to_string());
            }
        }
        out
    }

    /// Emit completed tool calls, usage and the stop event (idempotent).
    pub fn finish(&mut self) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        self.done = true;
        let had_calls = !self.calls.is_empty();
        for (idx, c) in std::mem::take(&mut self.calls) {
            let arguments = if c.arguments.trim().is_empty() {
                json!({})
            } else {
                match serde_json::from_str(&c.arguments) {
                    Ok(v) => v,
                    Err(_) => json!({"_raw": c.arguments}),
                }
            };
            let id = if c.id.is_empty() {
                format!("call_{idx}")
            } else {
                c.id
            };
            out.push(ModelEvent::ToolCall {
                id,
                name: c.name,
                arguments,
            });
        }
        if let Some((i, o)) = self.usage.take() {
            out.push(ModelEvent::Usage {
                input_tokens: i,
                output_tokens: o,
            });
        }
        let reason = match self.finish_reason.as_deref() {
            Some("tool_calls") | Some("function_call") => StopReason::ToolCalls,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Error,
            _ if had_calls => StopReason::ToolCalls,
            _ => StopReason::End,
        };
        out.push(ModelEvent::Stop {
            reason,
            message: None,
        });
        out
    }
}

/// Parse a complete SSE document into the event sequence.
pub fn parse_sse_document(doc: &str) -> Vec<ModelEvent> {
    let mut st = StreamState::new();
    let mut out = Vec::new();
    for ev in crate::sse::parse_all(doc) {
        out.extend(st.feed(&ev.data));
    }
    out.extend(st.finish());
    out
}

impl Decoder for StreamState {
    fn feed(&mut self, ev: &SseEvent) -> Vec<ModelEvent> {
        StreamState::feed(self, &ev.data)
    }
    fn finish(&mut self) -> Vec<ModelEvent> {
        StreamState::finish(self)
    }
    fn refused(&self) -> bool {
        self.finish_reason.as_deref() == Some("content_filter")
    }
    fn error_type(&self) -> Option<String> {
        self.error_type.clone()
    }
}

pub struct OpenAiCompatClient {
    ep: Arc<Endpoint>,
}

impl OpenAiCompatClient {
    /// A client with bearer auth when a key is given (Analyst's constructor). The caller
    /// allows `base_url` at the network gate (`wb_net::gate().allow_model_endpoint`).
    pub fn new(base_url: String, api_key: Option<String>, model: String) -> anyhow::Result<Self> {
        Self::from_endpoint(EndpointConfig {
            provider: String::new(),
            location: Default::default(),
            base_url,
            auth: if api_key.is_some() {
                AuthKind::Bearer
            } else {
                AuthKind::None
            },
            key: api_key,
            headers: BTreeMap::new(),
            model,
            audit: None,
            capabilities: None,
            capability_cache: None,
        })
    }

    pub fn from_endpoint(cfg: EndpointConfig) -> anyhow::Result<Self> {
        Ok(OpenAiCompatClient {
            ep: Arc::new(Endpoint::new(cfg)?),
        })
    }

    pub fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.ep.base_url)
    }

    pub fn models_url(&self) -> String {
        format!("{}/models", self.ep.base_url)
    }

    /// `GET {base_url}/models`, raw JSON.
    pub async fn list_models(&self) -> anyhow::Result<Value> {
        self.ep.get_json(self.models_url(), "probe", &[]).await
    }
}

impl ModelClient for OpenAiCompatClient {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ModelEvent> {
        let spec = WireSpec {
            url: self.chat_url(),
            fixed_headers: FIXED_HEADERS,
            build: build_for,
            optional: OPTIONAL,
        };
        wire::run_stream::<StreamState>(self.ep.clone(), spec, req)
    }

    fn model_id(&self) -> &str {
        &self.ep.model
    }

    fn apply_capabilities(&self, caps: &ModelCapabilities) {
        self.ep.features.replace(Features::from_capabilities(caps));
    }

    fn unsupported_fields(&self) -> Vec<String> {
        self.ep
            .features
            .snapshot()
            .unsupported
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, ToolDef};

    const TOOL_FIXTURE: &str = include_str!("fixtures/openai_tool_call.sse");
    const TEXT_FIXTURE: &str = include_str!("fixtures/openai_text.sse");

    #[test]
    fn tool_call_fixture_yields_exact_sequence() {
        let events = parse_sse_document(TOOL_FIXTURE);
        assert_eq!(
            events,
            vec![
                ModelEvent::Text {
                    delta: "Let me read that file.".into()
                },
                ModelEvent::ToolCall {
                    id: "call_abc123".into(),
                    name: "fs_read".into(),
                    arguments: json!({"path": "scripts/fit.py", "start_line": 1}),
                },
                ModelEvent::ToolCall {
                    id: "call_def456".into(),
                    name: "fs_list".into(),
                    arguments: json!({}),
                },
                ModelEvent::Usage {
                    input_tokens: 120,
                    output_tokens: 34
                },
                ModelEvent::Stop {
                    reason: StopReason::ToolCalls,
                    message: None
                },
            ]
        );
    }

    #[test]
    fn text_fixture_yields_text_and_end() {
        let events = parse_sse_document(TEXT_FIXTURE);
        assert_eq!(
            events,
            vec![
                ModelEvent::Text {
                    delta: "Hello".into()
                },
                ModelEvent::Text {
                    delta: ", world".into()
                },
                ModelEvent::Text { delta: "!".into() },
                ModelEvent::Usage {
                    input_tokens: 10,
                    output_tokens: 3
                },
                ModelEvent::Stop {
                    reason: StopReason::End,
                    message: None
                },
            ]
        );
    }

    #[test]
    fn error_payload_becomes_stop_error() {
        let events = parse_sse_document("data: {\"error\":{\"message\":\"boom\"}}\n\n");
        assert_eq!(
            events,
            vec![ModelEvent::Stop {
                reason: StopReason::Error,
                message: Some("boom".into())
            }]
        );
    }

    #[test]
    fn request_body_construction() {
        let req = ChatRequest {
            model: "m".into(),
            system: Some("sys".into()),
            messages: vec![
                Message::user_text("hi"),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentPart::Text {
                            text: "reading".into(),
                        },
                        ContentPart::ToolCall {
                            id: "c1".into(),
                            name: "fs_read".into(),
                            arguments: json!({"path":"a"}),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentPart::ToolResult {
                            tool_call_id: "c1".into(),
                            content: "{\"content\":\"x\"}".into(),
                            is_error: false,
                        },
                        ContentPart::Text { text: "see".into() },
                        ContentPart::Image {
                            media_type: "image/png".into(),
                            data: "AAAA".into(),
                        },
                    ],
                },
            ],
            tools: vec![ToolDef {
                name: "fs_read".into(),
                description: "read".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            }],
            max_tokens: Some(100),
            temperature: None,
            audit_tag: None,
        };
        let body = build_body(&req);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 100);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0], json!({"role":"system","content":"sys"}));
        assert_eq!(msgs[1], json!({"role":"user","content":"hi"}));
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "reading");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "c1");
        assert_eq!(msgs[2]["tool_calls"][0]["type"], "function");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "fs_read");
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert_eq!(
            msgs[3],
            json!({"role":"tool","tool_call_id":"c1","content":"{\"content\":\"x\"}"})
        );
        assert_eq!(msgs[4]["role"], "user");
        assert_eq!(msgs[4]["content"][0], json!({"type":"text","text":"see"}));
        assert_eq!(
            msgs[4]["content"][1],
            json!({"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}})
        );
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "fs_read");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn stream_options_downgrade_and_document_note() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    ContentPart::Document {
                        media_type: "application/pdf".into(),
                        data: "JVBERi0=".into(),
                        title: Some("Annual report".into()),
                    },
                    ContentPart::Text {
                        text: "Summarize.".into(),
                    },
                ],
            }],
            ..Default::default()
        };
        let body = build_body(&req);
        assert_eq!(
            wire::rejected_fields(
                OPTIONAL,
                &body,
                r#"{"detail":"Unrecognized request argument supplied: stream_options"}"#
            ),
            vec![FIELD_STREAM_OPTIONS]
        );
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert!(content.contains("Annual report") && content.ends_with("Summarize."));
        assert!(!content.contains("JVBERi0="));

        let mut features = Features::default();
        features.unsupported.insert(FIELD_STREAM_OPTIONS.into());
        let without = build_for(&req, &features);
        assert!(without.get("stream_options").is_none());
        assert!(
            wire::rejected_fields(OPTIONAL, &without, "unknown field stream_options").is_empty()
        );
    }
}
