//! Anthropic Messages adapter (PROTOCOL 4).

use super::{
    channel_stream, egress_message, gated, generated_call_id, normalize_messages, pump,
    send_with_retry, unparsed_arguments, HttpPolicy, ModelClient, SseState,
};
use analyst_protocol::model::{ChatRequest, ContentPart, ModelEvent, Role, StopReason};
use futures::stream::BoxStream;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use wb_net::reqwest;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = analyst_protocol::config::DEFAULT_MAX_OUTPUT_TOKENS;

/// Build the JSON request body. Roles strictly alternate (adjacent same-role messages are
/// merged), whitespace-only text is dropped, tool results lead their user message, and no
/// `temperature` is sent (current models reject it).
pub fn build_body(req: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for m in &normalize_messages(&req.messages) {
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
            .collect();
        // The API requires tool_result blocks first in the message that answers tool_use.
        let (results, others): (Vec<Value>, Vec<Value>) =
            blocks.into_iter().partition(|b| b["type"] == "tool_result");
        let blocks: Vec<Value> = results.into_iter().chain(others).collect();
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
            body["system"] = json!(s);
        }
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.parameters}))
                .collect(),
        );
    }
    body
}

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
    /// `message_stop` (or `error`) was received.
    terminal: bool,
    done: bool,
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
                    self.input_tokens = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
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
                        out.push(Self::tool_call(b, false));
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
                    if let Some(i) = u.get("input_tokens").and_then(Value::as_u64) {
                        self.input_tokens = i;
                    }
                }
            }
            "message_stop" => {
                self.terminal = true;
                out.extend(self.finish());
            }
            "error" => {
                self.terminal = true;
                self.done = true;
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

    fn tool_call(b: Block, cut_off: bool) -> ModelEvent {
        let arguments = if b.json.trim().is_empty() && !cut_off {
            json!({})
        } else {
            serde_json::from_str::<Value>(&b.json)
                .ok()
                .filter(|v| v.is_object() && !cut_off)
                .unwrap_or_else(|| unparsed_arguments(&b.json, cut_off))
        };
        ModelEvent::ToolCall {
            id: if b.id.is_empty() {
                generated_call_id()
            } else {
                b.id
            },
            name: b.name,
            arguments,
        }
    }

    /// The body ended. Without `message_stop` the response is incomplete: report an error
    /// instead of pretending the turn ended normally (B6).
    pub fn finish_eof(&mut self) -> Vec<ModelEvent> {
        if self.done || self.terminal {
            return self.finish();
        }
        self.done = true;
        vec![ModelEvent::Stop {
            reason: StopReason::Error,
            message: Some(
                "the model stream ended before the response was complete (connection dropped?)"
                    .into(),
            ),
        }]
    }

    /// Emit usage and the stop event (idempotent).
    pub fn finish(&mut self) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        self.done = true;
        // A tool_use block still open here was cut off by the output limit.
        let cut_off = self.stop_reason.as_deref() == Some("max_tokens");
        for (_, b) in std::mem::take(&mut self.blocks) {
            if b.kind == "tool_use" {
                out.push(Self::tool_call(b, cut_off));
            }
        }
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
    for ev in super::sse::parse_all(doc) {
        out.extend(st.feed(&ev.event, &ev.data));
    }
    out.extend(st.finish_eof());
    out
}

impl SseState for StreamState {
    fn on_event(&mut self, ev: &super::sse::SseEvent) -> Vec<ModelEvent> {
        self.feed(&ev.event, &ev.data)
    }
    fn on_eof(&mut self) -> Vec<ModelEvent> {
        self.finish_eof()
    }
}

pub struct AnthropicClient {
    base_url: String,
    api_key: String,
    model: String,
    pub policy: HttpPolicy,
}

impl AnthropicClient {
    pub fn new(base_url: String, api_key: String, model: String) -> anyhow::Result<Self> {
        let policy = HttpPolicy::default();
        Ok(AnthropicClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
            policy,
        })
    }

    pub fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    /// `GET {base_url}/v1/models/{id}`, raw JSON (`max_input_tokens` is the context window).
    pub async fn model_info(&self) -> anyhow::Result<Value> {
        let url = format!("{}/v1/models/{}", self.base_url, self.model);
        Ok(gated(reqwest::Method::GET, &url, "probe")
            .map_err(|e| anyhow::anyhow!(egress_message(&e)))?
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

impl ModelClient for AnthropicClient {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ModelEvent> {
        let body = build_body(&req);
        let url = self.messages_url();
        let api_key = self.api_key.clone();
        let policy = self.policy.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            let make = || {
                Ok(gated(reqwest::Method::POST, &url, "chat")?
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json")
                    .json(&body))
            };
            match send_with_retry(make, &policy).await {
                Ok(resp) => pump(resp, tx, policy.idle_timeout, StreamState::new()).await,
                Err(e) => {
                    let _ = tx
                        .send(ModelEvent::Stop {
                            reason: StopReason::Error,
                            message: Some(e.message()),
                        })
                        .await;
                }
            }
        });
        channel_stream(rx)
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analyst_protocol::model::{Message, ToolDef};

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
        };
        let body = build_body(&req);
        assert_eq!(body["system"], "sys");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 16384);
        assert!(body.get("temperature").is_none());
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

    /// B6: a body that ends without `message_stop` is an error, not a normal end.
    #[test]
    fn eof_before_message_stop_is_an_error() {
        let doc = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n";
        let events = parse_sse_document(doc);
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Stop { reason: StopReason::Error, message: Some(m) }) if m.contains("ended before")
        ));
    }

    /// B6: a tool call cut off by the output limit is flagged, never executed as garbage.
    #[test]
    fn tool_call_cut_off_by_max_tokens_is_flagged() {
        let doc = concat!(
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"fs_propose_create\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"a.py\\\", \\\"content\\\": \\\"x = \"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":16384}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let events = parse_sse_document(doc);
        match &events[0] {
            ModelEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "toolu_9");
                assert_eq!(name, "fs_propose_create");
                assert_eq!(arguments["_truncated"], true);
                assert!(arguments["_raw"].as_str().unwrap().contains("a.py"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Stop {
                reason: StopReason::MaxTokens,
                ..
            })
        ));
    }

    /// A1 and A4 on the wire: merged roles, tool results first, no blank text blocks.
    #[test]
    fn body_merges_same_role_messages_and_drops_blank_text() {
        let req = ChatRequest {
            model: "m".into(),
            system: None,
            messages: vec![
                Message::user_text("a"),
                Message::assistant_text(" \n"),
                Message::user_text("b"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::ToolCall {
                        id: "t1".into(),
                        name: "x".into(),
                        arguments: json!({}),
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentPart::Text {
                            text: "note".into(),
                        },
                        ContentPart::ToolResult {
                            tool_call_id: "t1".into(),
                            content: "ok".into(),
                            is_error: false,
                        },
                    ],
                },
            ],
            tools: vec![],
            max_tokens: Some(5),
        };
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, vec!["user", "assistant", "user"]);
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][1]["type"], "text");
        let text = serde_json::to_string(&body).unwrap();
        assert!(!text.contains("\"text\":\" \\n\""));
    }

    /// A2: overload and rate-limit responses are retried before the first byte.
    #[tokio::test]
    async fn retries_overload_then_streams() {
        use crate::providers::testserver::{serve, Canned};
        let ok = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"hi\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let server = serve(vec![
            Canned::status(
                529,
                "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}",
            ),
            Canned::status(429, "slow down"),
            Canned::sse(ok),
        ])
        .await;
        let mut c = AnthropicClient::new(server.url.clone(), "k".into(), "m".into()).unwrap();
        c.policy.backoff_base = std::time::Duration::from_millis(5);
        let got = crate::providers::collect(c.stream(ChatRequest {
            model: "m".into(),
            messages: vec![Message::user_text("x")],
            ..Default::default()
        }))
        .await;
        assert_eq!(got.stop, Some(StopReason::End), "{:?}", got.stop_message);
        assert_eq!(got.text, "hi");
        assert_eq!(server.bodies().len(), 3);
        // A 400 is not retried.
        let server = serve(vec![Canned::status(400, "bad request body")]).await;
        let mut c = AnthropicClient::new(server.url.clone(), "k".into(), "m".into()).unwrap();
        c.policy.backoff_base = std::time::Duration::from_millis(5);
        let got = crate::providers::collect(c.stream(ChatRequest::default())).await;
        assert_eq!(got.stop, Some(StopReason::Error));
        assert!(got.stop_message.unwrap().contains("HTTP 400"));
        assert_eq!(server.bodies().len(), 1);
    }
}
