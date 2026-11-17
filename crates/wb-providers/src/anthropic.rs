// Ported from Personal Analyst `crates/core/src/providers/anthropic.rs` at commit f229e4d.
// Adapted (Present PROTOCOL 6.1): configurable auth and extra headers, document blocks with
// citations, prompt-caching markers, the unknown-field downgrade, cache tokens in usage, and
// the audit hook (the request pump now lives in `wire.rs`).
//! Anthropic Messages adapter.

use crate::config::AuthKind;
use crate::model::{ChatRequest, ContentPart, ModelCapabilities, ModelEvent, Role, StopReason};
use crate::sse::SseEvent;
use crate::wire::{
    self, Decoder, Endpoint, EndpointConfig, Features, OptionalField, WireFeatures, WireSpec,
    FIELD_CACHE_CONTROL, FIELD_CITATIONS, FIELD_DOCUMENT,
};
use crate::ModelClient;
use futures::stream::BoxStream;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

const FIXED_HEADERS: &[(&str, &str)] = &[
    ("anthropic-version", ANTHROPIC_VERSION),
    ("content-type", "application/json"),
];

/// Which optional request features a body carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyOptions {
    /// `cache_control: {type: "ephemeral"}` on the last system block and last tool.
    pub caching: bool,
    /// Send `Document` parts as native document blocks (otherwise as a text note).
    pub documents: bool,
    /// `citations: {enabled: true}` on document blocks.
    pub citations: bool,
}

impl Default for BodyOptions {
    /// The plain wire mapping: documents with citations, no caching markers.
    fn default() -> Self {
        BodyOptions {
            caching: false,
            documents: true,
            citations: true,
        }
    }
}

impl BodyOptions {
    fn from_features(f: &Features) -> Self {
        BodyOptions {
            caching: f.caching && f.allows(FIELD_CACHE_CONTROL),
            documents: f.documents && f.allows(FIELD_DOCUMENT),
            citations: f.allows(FIELD_CITATIONS),
        }
    }
}

/// The text sent in place of a document the endpoint cannot take.
pub(crate) fn document_note(title: Option<&str>) -> String {
    match title {
        Some(t) => {
            format!("[document \"{t}\" omitted: this endpoint does not accept document blocks]")
        }
        None => "[document omitted: this endpoint does not accept document blocks]".to_string(),
    }
}

/// Build the JSON request body with the plain wire mapping.
pub fn build_body(req: &ChatRequest) -> Value {
    build_body_with(req, &BodyOptions::default())
}

/// Build the JSON request body.
pub fn build_body_with(req: &ChatRequest, opts: &BodyOptions) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let blocks: Vec<Value> = m
            .content
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => json!({"type":"text","text":text}),
                ContentPart::Image { media_type, data } => json!({
                    "type":"image",
                    "source":{"type":"base64","media_type":media_type,"data":data}
                }),
                ContentPart::Document {
                    media_type,
                    data,
                    title,
                } => {
                    if !opts.documents {
                        return json!({"type":"text","text":document_note(title.as_deref())});
                    }
                    let mut b = json!({
                        "type":"document",
                        "source":{"type":"base64","media_type":media_type,"data":data}
                    });
                    if let Some(t) = title {
                        b["title"] = json!(t);
                    }
                    if opts.citations {
                        b["citations"] = json!({"enabled": true});
                    }
                    b
                }
                ContentPart::ToolCall {
                    id,
                    name,
                    arguments,
                } => json!({"type":"tool_use","id":id,"name":name,"input":arguments}),
                ContentPart::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    let mut b =
                        json!({"type":"tool_result","tool_use_id":tool_call_id,"content":content});
                    if *is_error {
                        b["is_error"] = json!(true);
                    }
                    b
                }
            })
            .filter(|b| !(b["type"] == "text" && b["text"].as_str() == Some("")))
            .collect();
        if blocks.is_empty() {
            continue;
        }
        messages.push(json!({"role": role, "content": blocks}));
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": true,
    });
    if let Some(s) = &req.system {
        if !s.is_empty() {
            body["system"] = if opts.caching {
                json!([{"type":"text","text":s,"cache_control":{"type":"ephemeral"}}])
            } else {
                json!(s)
            };
        }
    }
    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.parameters}))
            .collect();
        if opts.caching {
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = json!({"type":"ephemeral"});
            }
        }
        body["tools"] = Value::Array(tools);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    body
}

fn build_for(req: &ChatRequest, features: &Features) -> Value {
    build_body_with(req, &BodyOptions::from_features(features))
}

fn sends_document(body: &Value) -> bool {
    body["messages"].as_array().is_some_and(|ms| {
        ms.iter().any(|m| {
            m["content"]
                .as_array()
                .is_some_and(|bs| bs.iter().any(|b| b["type"] == "document"))
        })
    })
}

/// Phrases by which endpoints reject a field they do not know. Required in addition to the
/// name for fields whose names are ordinary words, so that an unrelated 400 that merely
/// mentions a document (too many pages, say) does not switch documents off.
fn says_rejected(lower: &str) -> bool {
    [
        "extra inputs",
        "not permitted",
        "not allowed",
        "unknown",
        "unsupported",
        "not supported",
        "unrecognized",
        "unrecognised",
        "unexpected",
        "does not match",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

static OPTIONAL: &[OptionalField] = &[
    OptionalField {
        name: FIELD_CACHE_CONTROL,
        sent: |b| wire::has_key(b, "cache_control"),
        named: |t| t.contains("cache_control"),
    },
    OptionalField {
        name: FIELD_CITATIONS,
        sent: |b| wire::has_key(&b["messages"], "citations"),
        named: |t| t.contains("citations") && says_rejected(t),
    },
    OptionalField {
        name: FIELD_DOCUMENT,
        sent: sends_document,
        named: |t| t.contains("document") && says_rejected(t),
    },
];

#[derive(Debug, Default)]
struct Block {
    kind: String,
    id: String,
    name: String,
    json: String,
}

/// Incremental state for one streamed response.
#[derive(Debug, Default)]
pub struct StreamState {
    blocks: BTreeMap<u64, Block>,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<String>,
    error_type: Option<String>,
    done: bool,
}

/// Total prompt tokens of a usage object: `input_tokens` plus the cache-write and cache-read
/// counts the API reports separately. `None` when `input_tokens` is absent.
fn input_total(u: &Value) -> Option<u64> {
    let base = u.get("input_tokens").and_then(Value::as_u64)?;
    let extra = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(base + extra("cache_creation_input_tokens") + extra("cache_read_input_tokens"))
}

fn page_citation(c: &Value) -> Option<ModelEvent> {
    if c.get("type").and_then(Value::as_str) != Some("page_location") {
        return None;
    }
    let n = |k: &str| c.get(k).and_then(Value::as_u64).map(|v| v as u32);
    Some(ModelEvent::Citation {
        document_index: n("document_index")?,
        start_page: n("start_page_number")?,
        end_page: n("end_page_number")?,
        cited_text: c
            .get("cited_text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

impl StreamState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one SSE event (`event` name and `data` payload).
    pub fn feed(&mut self, event: &str, data: &str) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        if self.done {
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
        let kind = if event.is_empty() {
            v.get("type").and_then(Value::as_str).unwrap_or("")
        } else {
            event
        };
        match kind {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.input_tokens = input_total(u).unwrap_or(0);
                    self.output_tokens =
                        u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                }
            }
            "content_block_start" => {
                let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                let cb = v.get("content_block").cloned().unwrap_or(Value::Null);
                let b = Block {
                    kind: cb
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    id: cb
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    name: cb
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    json: String::new(),
                };
                if b.kind == "text" {
                    if let Some(t) = cb.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(ModelEvent::Text {
                                delta: t.to_string(),
                            });
                        }
                    }
                }
                self.blocks.insert(idx, b);
            }
            "content_block_delta" => {
                let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(d) = v.get("delta") {
                    match d.get("type").and_then(Value::as_str) {
                        Some("text_delta") => {
                            if let Some(t) = d.get("text").and_then(Value::as_str) {
                                if !t.is_empty() {
                                    out.push(ModelEvent::Text {
                                        delta: t.to_string(),
                                    });
                                }
                            }
                        }
                        Some("citations_delta") => {
                            // Only page locations (PDF documents) are mapped; other
                            // citation kinds are ignored.
                            if let Some(c) = d.get("citation").and_then(page_citation) {
                                out.push(c);
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(p) = d.get("partial_json").and_then(Value::as_str) {
                                self.blocks.entry(idx).or_default().json.push_str(p);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(b) = self.blocks.remove(&idx) {
                    if b.kind == "tool_use" {
                        let arguments = if b.json.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(&b.json)
                                .unwrap_or_else(|_| json!({"_raw": b.json}))
                        };
                        out.push(ModelEvent::ToolCall {
                            id: b.id,
                            name: b.name,
                            arguments,
                        });
                    }
                }
            }
            "message_delta" => {
                if let Some(sr) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(sr.to_string());
                }
                if let Some(u) = v.get("usage") {
                    if let Some(o) = u.get("output_tokens").and_then(Value::as_u64) {
                        self.output_tokens = o;
                    }
                    if let Some(i) = input_total(u) {
                        self.input_tokens = i;
                    }
                }
            }
            "message_stop" => out.extend(self.finish()),
            "error" => {
                self.done = true;
                self.error_type = v
                    .pointer("/error/type")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("provider error")
                    .to_string();
                out.push(ModelEvent::Stop {
                    reason: StopReason::Error,
                    message: Some(msg),
                });
            }
            _ => {} // ping and unknown events
        }
        out
    }

    /// Emit usage and the stop event (idempotent).
    pub fn finish(&mut self) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        self.done = true;
        out.push(ModelEvent::Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        });
        let reason = match self.stop_reason.as_deref() {
            Some("tool_use") => StopReason::ToolCalls,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("end_turn") | Some("stop_sequence") | None => StopReason::End,
            Some(_) => StopReason::End,
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
        out.extend(st.feed(&ev.event, &ev.data));
    }
    out.extend(st.finish());
    out
}

impl Decoder for StreamState {
    fn feed(&mut self, ev: &SseEvent) -> Vec<ModelEvent> {
        StreamState::feed(self, &ev.event, &ev.data)
    }
    fn finish(&mut self) -> Vec<ModelEvent> {
        StreamState::finish(self)
    }
    fn refused(&self) -> bool {
        self.stop_reason.as_deref() == Some("refusal")
    }
    fn error_type(&self) -> Option<String> {
        self.error_type.clone()
    }
}

pub struct AnthropicClient {
    ep: Arc<Endpoint>,
}

impl AnthropicClient {
    /// A client with `x-api-key` auth and default network settings (Analyst's constructor).
    pub fn new(base_url: String, api_key: String, model: String) -> anyhow::Result<Self> {
        Self::from_endpoint(EndpointConfig {
            provider: String::new(),
            location: Default::default(),
            base_url,
            auth: AuthKind::XApiKey,
            key: Some(api_key),
            headers: BTreeMap::new(),
            model,
            http: reqwest::Client::builder().build()?,
            audit: None,
            capabilities: None,
            capability_cache: None,
        })
    }

    pub fn from_endpoint(cfg: EndpointConfig) -> anyhow::Result<Self> {
        Ok(AnthropicClient {
            ep: Arc::new(Endpoint::new(cfg)?),
        })
    }

    pub fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.ep.base_url)
    }

    /// `GET {base_url}/v1/models/{id}`, raw JSON (used for the capability probe).
    pub async fn model_info(&self) -> anyhow::Result<Value> {
        let url = format!("{}/v1/models/{}", self.ep.base_url, self.ep.model);
        self.ep.get_json(url, FIXED_HEADERS).await
    }
}

impl ModelClient for AnthropicClient {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ModelEvent> {
        let spec = WireSpec {
            url: self.messages_url(),
            fixed_headers: FIXED_HEADERS,
            build: build_for,
            optional: OPTIONAL,
        };
        wire::run_stream::<StreamState>(self.ep.clone(), spec, req)
    }

    fn model_id(&self) -> &str {
        &self.ep.model
    }

    fn wire_features(&self) -> WireFeatures {
        WireFeatures {
            documents: true,
            caching: true,
        }
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

    const TOOL_FIXTURE: &str = include_str!("fixtures/anthropic_tool_use.sse");
    const TEXT_FIXTURE: &str = include_str!("fixtures/anthropic_text.sse");

    #[test]
    fn tool_use_fixture_yields_exact_sequence() {
        let events = parse_sse_document(TOOL_FIXTURE);
        assert_eq!(
            events,
            vec![
                ModelEvent::Text {
                    delta: "I'll check the script".into()
                },
                ModelEvent::Text {
                    delta: " index first.".into()
                },
                ModelEvent::ToolCall {
                    id: "toolu_01A".into(),
                    name: "scripts_index".into(),
                    arguments: json!({}),
                },
                ModelEvent::ToolCall {
                    id: "toolu_01B".into(),
                    name: "fs_read".into(),
                    arguments: json!({"path": "analysis/fitting.py", "end_line": 40}),
                },
                ModelEvent::Usage {
                    input_tokens: 250,
                    output_tokens: 61
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
                    delta: "The fit".into()
                },
                ModelEvent::Text {
                    delta: " converged.".into()
                },
                ModelEvent::Usage {
                    input_tokens: 40,
                    output_tokens: 8
                },
                ModelEvent::Stop {
                    reason: StopReason::End,
                    message: None
                },
            ]
        );
    }

    #[test]
    fn error_event_becomes_stop_error() {
        let doc = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        assert_eq!(
            parse_sse_document(doc),
            vec![ModelEvent::Stop {
                reason: StopReason::Error,
                message: Some("Overloaded".into())
            }]
        );
    }

    #[test]
    fn request_body_construction() {
        let req = ChatRequest {
            model: "claude-x".into(),
            system: Some("sys".into()),
            messages: vec![
                Message::user_text("hi"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::ToolCall {
                        id: "toolu_1".into(),
                        name: "fs_read".into(),
                        arguments: json!({"path":"a"}),
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentPart::ToolResult {
                            tool_call_id: "toolu_1".into(),
                            content: "ok".into(),
                            is_error: true,
                        },
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
                parameters: json!({"type":"object"}),
            }],
            max_tokens: None,
            temperature: Some(0.0),
            audit_tag: None,
        };
        let body = build_body(&req);
        assert_eq!(body["system"], "sys");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["tools"][0]["name"], "fs_read");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(
            msgs[0],
            json!({"role":"user","content":[{"type":"text","text":"hi"}]})
        );
        assert_eq!(
            msgs[1],
            json!({"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"fs_read","input":{"path":"a"}}]})
        );
        assert_eq!(
            msgs[2]["content"][0],
            json!({"type":"tool_result","tool_use_id":"toolu_1","content":"ok","is_error":true})
        );
        assert_eq!(
            msgs[2]["content"][1],
            json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}})
        );
        assert!(body
            .get("messages")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m.get("role").is_some()));
    }

    const CITATIONS_FIXTURE: &str = include_str!("fixtures/anthropic_citations.sse");

    #[test]
    fn citations_fixture_maps_page_locations() {
        let events = parse_sse_document(CITATIONS_FIXTURE);
        assert_eq!(
            events,
            vec![
                ModelEvent::Text {
                    delta: "Revenue grew ".into()
                },
                ModelEvent::Text {
                    delta: "12% in 2025".into()
                },
                ModelEvent::Citation {
                    document_index: 0,
                    start_page: 3,
                    end_page: 4,
                    cited_text: "Revenue increased 12% year over year.".into(),
                },
                ModelEvent::Text {
                    delta: " and margins held at ".into()
                },
                ModelEvent::Text {
                    delta: "31%".into()
                },
                ModelEvent::Citation {
                    document_index: 1,
                    start_page: 7,
                    end_page: 9,
                    cited_text: "Operating margin: 31%".into(),
                },
                ModelEvent::Text { delta: ".".into() },
                ModelEvent::Usage {
                    input_tokens: 5200,
                    output_tokens: 24
                },
                ModelEvent::Stop {
                    reason: StopReason::End,
                    message: None
                },
            ]
        );
    }

    #[test]
    fn non_page_citations_are_ignored() {
        let mut st = StreamState::new();
        let evs = st.feed(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"char_location","cited_text":"x","document_index":0,"start_char_index":0,"end_char_index":1}}}"#,
        );
        assert!(evs.is_empty());
    }

    #[test]
    fn usage_includes_cache_tokens() {
        let mut st = StreamState::new();
        st.feed(
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_creation_input_tokens":200,"cache_read_input_tokens":3000,"output_tokens":1}}}"#,
        );
        st.feed(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        );
        assert_eq!(
            st.finish()[0],
            ModelEvent::Usage {
                input_tokens: 3210,
                output_tokens: 7
            }
        );
    }

    fn doc_request() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: Some("sys".into()),
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    ContentPart::Document {
                        media_type: "application/pdf".into(),
                        data: "JVBERi0=".into(),
                        title: Some("Annual report".into()),
                    },
                    ContentPart::Document {
                        media_type: "application/pdf".into(),
                        data: "JVBERi0=".into(),
                        title: None,
                    },
                    ContentPart::Text {
                        text: "Summarize.".into(),
                    },
                ],
            }],
            tools: vec![
                ToolDef {
                    name: "a".into(),
                    description: "first".into(),
                    parameters: json!({"type":"object"}),
                },
                ToolDef {
                    name: "b".into(),
                    description: "last".into(),
                    parameters: json!({"type":"object"}),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn document_blocks_and_citations() {
        let body = build_body(&doc_request());
        let content = &body["messages"][0]["content"];
        assert_eq!(
            content[0],
            json!({
                "type": "document",
                "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERi0="},
                "title": "Annual report",
                "citations": {"enabled": true}
            })
        );
        assert!(content[1].get("title").is_none());
        assert_eq!(content[1]["citations"]["enabled"], true);

        let no_cite = build_body_with(
            &doc_request(),
            &BodyOptions {
                citations: false,
                ..Default::default()
            },
        );
        assert!(!wire::has_key(&no_cite, "citations"));
        assert!(sends_document(&no_cite));

        let no_docs = build_body_with(
            &doc_request(),
            &BodyOptions {
                documents: false,
                ..Default::default()
            },
        );
        assert!(!sends_document(&no_docs));
        assert_eq!(no_docs["messages"][0]["content"][0]["type"], "text");
        assert!(no_docs["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Annual report"));
    }

    #[test]
    fn caching_marks_last_system_block_and_last_tool_only() {
        let plain = build_body(&doc_request());
        assert_eq!(plain["system"], "sys");
        assert!(!wire::has_key(&plain, "cache_control"));

        let body = build_body_with(
            &doc_request(),
            &BodyOptions {
                caching: true,
                ..Default::default()
            },
        );
        assert_eq!(
            body["system"],
            json!([{"type":"text","text":"sys","cache_control":{"type":"ephemeral"}}])
        );
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(
            body["tools"][1]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert!(!wire::has_key(&body["messages"], "cache_control"));
    }

    #[test]
    fn options_follow_the_capability_record() {
        let mut caps = ModelCapabilities::default();
        let o = BodyOptions::from_features(&Features::from_capabilities(&caps));
        assert!(!o.caching && !o.documents && o.citations);
        caps.caching = true;
        caps.documents = true;
        let o = BodyOptions::from_features(&Features::from_capabilities(&caps));
        assert!(o.caching && o.documents && o.citations);
        caps.mark_unsupported(FIELD_CITATIONS);
        caps.mark_unsupported(FIELD_CACHE_CONTROL);
        let o = BodyOptions::from_features(&Features::from_capabilities(&caps));
        assert!(!o.caching && o.documents && !o.citations);
    }

    #[test]
    fn rejected_field_detection() {
        let all = BodyOptions {
            caching: true,
            ..Default::default()
        };
        let body = build_body_with(&doc_request(), &all);
        let r = |text: &str| wire::rejected_fields(OPTIONAL, &body, text);
        assert_eq!(
            r(r#"{"error":{"message":"system.0.cache_control: Extra inputs are not permitted"}}"#),
            vec![FIELD_CACHE_CONTROL]
        );
        assert_eq!(
            r("messages.0.content.0.document.citations: Extra inputs are not permitted"),
            vec![FIELD_CITATIONS],
            "the narrower field is dropped first"
        );
        assert_eq!(
            r("messages.0.content.0: Input tag 'document' found using 'type' does not match any of the expected tags"),
            vec![FIELD_DOCUMENT]
        );
        assert!(
            r("document has too many pages: maximum is 100").is_empty(),
            "a 400 about a document's content is not an unknown-field rejection"
        );
        assert!(r("max_tokens: must be positive").is_empty());

        // A field that was not sent is never blamed.
        let plain = build_body(&ChatRequest {
            model: "m".into(),
            messages: vec![Message::user_text("hi")],
            ..Default::default()
        });
        assert!(wire::rejected_fields(OPTIONAL, &plain, "unknown field cache_control").is_empty());
    }
}
