// Ported from Fibration Lab `crates/core/src/providers/mod.rs` at commit f229e4d.
// Adapted: a self-contained crate (owns its request, event, and config types); `client_for`
// takes the provider entry plus key store, audit sink, and capability cache;
// `ModelClient` gains optional capability hooks; `collect` gathers citations.
//! Shared-to-be crate `wb-providers` (BUILD-SPEC section 4): one `ModelClient` trait, two
//! wire adapters (OpenAI-compatible chat completions and Anthropic Messages), SSE parsing,
//! capability probing (native tool calling is required), and gateway support: configurable auth,
//! extra headers, key lookup, the unknown-field downgrade, and an audit hook (Present
//! PROTOCOL 4.1, 6.1, 10).
//!
//! This crate builds no HTTP client. Every request is built by the network gate
//! (`wb_net::gate().request`), which refuses any destination that is not an allowed model
//! endpoint; `client_for` allows the provider's `base_url` there. Keys are read from the
//! process environment or a `.env` file (`keys`).
//!
//! Must not depend on Present-specific crates. Nothing here is conditioned on a model or
//! provider name; behavior follows the provider `kind` (wire format) and the capability
//! record only.

pub mod anthropic;
pub mod audit;
pub mod capabilities;
pub mod config;
pub mod keys;
pub mod model;
pub mod names;
pub mod openai_compat;
pub mod probe_media;
pub mod sse;
pub mod wire;

pub use audit::{AuditRecord, AuditSink, AuditStatus};
pub use config::{AuthKind, Location, ModelConfig, ProviderConfig, ProviderKind, ToolSupport};
pub use keys::{KeyError, KeyRef, KeyStores};
pub use model::{
    ChatRequest, ContentPart, Message, ModelCapabilities, ModelEvent, Role, StopReason, ToolDef,
};
use wb_net::EgressError;
pub use wire::{CapabilityCacheRef, EndpointConfig, WireFeatures};

use futures::stream::BoxStream;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A streaming chat client. Every adapter produces exactly the `ModelEvent` sequence of
/// the wire contract and always terminates the stream with a `Stop` event.
pub trait ModelClient: Send + Sync {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ModelEvent>;
    /// Model id to put in requests and usage events.
    fn model_id(&self) -> &str;
    /// What this client's wire format can express beyond the common subset.
    fn wire_features(&self) -> WireFeatures {
        WireFeatures::default()
    }
    /// Shape later requests by this capability record (caching markers, document blocks,
    /// fields known to be rejected).
    fn apply_capabilities(&self, _caps: &ModelCapabilities) {}
    /// Optional request fields the endpoint has rejected so far.
    fn unsupported_fields(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What `client_for` needs besides the provider entry.
pub struct ClientOptions<'a> {
    pub keys: &'a KeyStores,
    pub audit: Option<Arc<dyn AuditSink>>,
    /// The application home holding `capabilities.json`. When given, the cached capability
    /// record for `provider/model` shapes requests and unknown-field downgrades are
    /// persisted to it.
    pub home: Option<&'a Path>,
}

/// Cache key of a model's capability record.
pub fn capability_key(provider: &str, model_id: &str) -> String {
    format!("{provider}/{model_id}")
}

/// Build the client for a model on a configured provider. The provider's base URL becomes
/// an allowed model endpoint at the network gate: this is how the allowlist is filled from
/// the user's configuration.
pub fn client_for(
    provider: &ProviderConfig,
    model_id: &str,
    opts: &ClientOptions<'_>,
) -> anyhow::Result<Arc<dyn ModelClient>> {
    let key = opts.keys.resolve(provider)?;
    let base_url = provider.effective_base_url();
    wb_net::gate()
        .allow_model_endpoint(&base_url)
        .map_err(|e| anyhow::anyhow!("provider `{}`: {}", provider.name, egress_message(&e)))?;
    let mut cfg = EndpointConfig::from_provider(provider, key, model_id);
    cfg.audit = opts.audit.clone();
    if let Some(home) = opts.home {
        let cache_key = capability_key(&provider.name, model_id);
        cfg.capabilities = capabilities::cached(home, &cache_key);
        cfg.capability_cache = Some(CapabilityCacheRef {
            home: home.to_path_buf(),
            key: cache_key,
        });
    }
    Ok(match provider.kind {
        ProviderKind::OpenAiCompatible => {
            Arc::new(openai_compat::OpenAiCompatClient::from_endpoint(cfg)?)
        }
        ProviderKind::Anthropic => Arc::new(anthropic::AnthropicClient::from_endpoint(cfg)?),
    })
}

/// The model's context window as reported by the provider, when it reports one: Anthropic
/// `GET /v1/models/{id}` (`max_input_tokens`), OpenAI-compatible `GET /models`
/// (`context_length`, `context_window`, `max_model_len`, ...). `None` on any failure,
/// including a missing key; the lookup is bounded to 10 seconds.
pub async fn model_info_window(
    provider: &ProviderConfig,
    model_id: &str,
    keys: &KeyStores,
) -> Option<u32> {
    let key = keys.resolve(provider).ok()?;
    wb_net::gate()
        .allow_model_endpoint(&provider.effective_base_url())
        .ok()?;
    let cfg = EndpointConfig::from_provider(provider, key, model_id);
    let lookup = async {
        match provider.kind {
            ProviderKind::OpenAiCompatible => openai_compat::OpenAiCompatClient::from_endpoint(cfg)
                .ok()?
                .list_models()
                .await
                .ok(),
            ProviderKind::Anthropic => anthropic::AnthropicClient::from_endpoint(cfg)
                .ok()?
                .model_info()
                .await
                .ok(),
        }
    };
    let info = tokio::time::timeout(Duration::from_secs(10), lookup)
        .await
        .ok()??;
    capabilities::context_window_from_info(&info, model_id)
}

/// History as both wire formats need it: text parts that are empty or whitespace-only are
/// dropped (the Anthropic API rejects them), messages left without content are dropped, and
/// adjacent messages of the same role are merged so roles strictly alternate.
pub fn normalize_messages(messages: &[Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for m in messages {
        let content: Vec<ContentPart> = m
            .content
            .iter()
            .filter(|p| !matches!(p, ContentPart::Text { text } if text.trim().is_empty()))
            .cloned()
            .collect();
        if content.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.role == m.role => {
                // Keep separate texts readable when two messages are joined.
                if let (Some(ContentPart::Text { text: a }), Some(ContentPart::Text { .. })) =
                    (last.content.last_mut(), content.first())
                {
                    if !a.ends_with('\n') {
                        a.push('\n');
                    }
                }
                last.content.extend(content);
            }
            _ => out.push(Message {
                role: m.role,
                content,
            }),
        }
    }
    out
}

/// Timeouts and retry policy for provider HTTP calls (Analyst PROTOCOL 9).
#[derive(Debug, Clone)]
pub struct HttpPolicy {
    /// Time allowed until response headers arrive.
    pub first_byte_timeout: Duration,
    /// Longest silence tolerated between stream chunks.
    pub idle_timeout: Duration,
    /// Total attempts before the first byte (1 = no retry).
    pub max_attempts: u32,
    /// First backoff delay; doubles per attempt.
    pub backoff_base: Duration,
}

impl Default for HttpPolicy {
    fn default() -> Self {
        HttpPolicy {
            first_byte_timeout: Duration::from_secs(300),
            idle_timeout: Duration::from_secs(180),
            max_attempts: 4,
            backoff_base: Duration::from_secs(1),
        }
    }
}

/// A refusal by the network gate, in words a user can act on.
pub fn egress_message(e: &EgressError) -> String {
    match e {
        EgressError::Denied { destination, .. } => format!(
            "the request was not sent: {destination} is not a configured model endpoint. The app only contacts the `base_url` of providers listed in config.toml; add or correct the provider there"
        ),
        EgressError::BadDestination(d) => format!(
            "the request was not sent: `{d}` is not a usable http(s) URL; check the provider's `base_url` in config.toml"
        ),
        EgressError::Ledger(why) => format!(
            "the request was not sent: the network ledger (egress.jsonl in the app home) could not be written ({why}). Nothing leaves the machine unrecorded; make the home directory writable"
        ),
        EgressError::Client(why) => format!("the HTTP client could not be built: {why}"),
    }
}

/// A unique id for a tool call whose provider did not supply one.
pub(crate) fn generated_call_id() -> String {
    format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..16])
}

/// Arguments for a tool call whose JSON could not be parsed. `_truncated` is set when the
/// response stopped at the output token limit, so the agent can tell the model to split the
/// edit instead of executing garbage.
pub(crate) fn unparsed_arguments(raw: &str, truncated: bool) -> serde_json::Value {
    let mut v = serde_json::json!({ "_raw": wire::truncate_bytes(raw, 2000) });
    if truncated {
        v["_truncated"] = serde_json::json!(true);
    }
    v
}

/// Turn a receiver of events into a boxed stream (adapters push from a spawned task).
pub(crate) fn channel_stream(
    rx: tokio::sync::mpsc::Receiver<ModelEvent>,
) -> BoxStream<'static, ModelEvent> {
    Box::pin(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|e| (e, rx))
    }))
}

/// A client that replays scripted event sequences; records every request it receives.
/// Used by agent, eval, and probe tests.
pub struct ScriptedModelClient {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    pub requests: Mutex<Vec<ChatRequest>>,
    model: String,
}

impl ScriptedModelClient {
    pub fn new(scripts: Vec<Vec<ModelEvent>>) -> Self {
        ScriptedModelClient {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(vec![]),
            model: "scripted/model".to_string(),
        }
    }

    /// Number of requests received so far.
    pub fn calls(&self) -> usize {
        self.requests.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Snapshot of the recorded requests.
    pub fn recorded(&self) -> Vec<ChatRequest> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl ModelClient for ScriptedModelClient {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ModelEvent> {
        if let Ok(mut r) = self.requests.lock() {
            r.push(req);
        }
        let mut events = self
            .scripts
            .lock()
            .ok()
            .and_then(|mut s| s.pop_front())
            .unwrap_or_default();
        if !matches!(events.last(), Some(ModelEvent::Stop { .. })) {
            events.push(ModelEvent::Stop {
                reason: StopReason::End,
                message: None,
            });
        }
        Box::pin(futures::stream::iter(events))
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

/// A citation gathered by `collect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedCitation {
    pub document_index: u32,
    pub start_page: u32,
    pub end_page: u32,
    pub cited_text: String,
    /// Byte length of the collected text when the citation arrived: the cited claim ends
    /// here.
    pub text_offset: usize,
}

/// Collect a full response from a stream: text, completed tool calls, citations, usage,
/// stop reason.
#[derive(Debug, Default, Clone)]
pub struct Collected {
    pub text: String,
    pub citations: Vec<CollectedCitation>,
    pub tool_calls: Vec<(String, String, serde_json::Value)>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub stop: Option<StopReason>,
    pub stop_message: Option<String>,
}

pub async fn collect(mut stream: BoxStream<'static, ModelEvent>) -> Collected {
    use futures::StreamExt;
    let mut c = Collected::default();
    while let Some(ev) = stream.next().await {
        match ev {
            ModelEvent::Text { delta } => c.text.push_str(&delta),
            ModelEvent::ToolCall {
                id,
                name,
                arguments,
            } => c.tool_calls.push((id, name, arguments)),
            ModelEvent::Citation {
                document_index,
                start_page,
                end_page,
                cited_text,
            } => c.citations.push(CollectedCitation {
                document_index,
                start_page,
                end_page,
                cited_text,
                text_offset: c.text.len(),
            }),
            ModelEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                c.input_tokens += input_tokens;
                c.output_tokens += output_tokens;
            }
            ModelEvent::Stop { reason, message } => {
                c.stop = Some(reason);
                c.stop_message = message;
                break;
            }
        }
    }
    c
}

#[cfg(test)]
pub(crate) mod testserver {
    //! A minimal scripted HTTP server for adapter tests: each connection gets the next
    //! canned response; request bodies are recorded.

    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub struct Canned {
        pub status: u16,
        pub headers: Vec<(&'static str, String)>,
        pub body: String,
    }

    impl Canned {
        pub fn sse(body: &str) -> Canned {
            Canned {
                status: 200,
                headers: vec![("content-type", "text/event-stream".into())],
                body: body.to_string(),
            }
        }
        pub fn status(status: u16, body: &str) -> Canned {
            Canned {
                status,
                headers: vec![],
                body: body.to_string(),
            }
        }
    }

    pub struct Server {
        pub url: String,
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        pub fn bodies(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while buf.len() < pos + 4 + len {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                return String::from_utf8_lossy(&buf[pos + 4..]).to_string();
            }
        }
        String::new()
    }

    pub async fn serve(responses: Vec<Canned>) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // Tests name their fake endpoint to the gate, as configuration does for real ones.
        wb_net::gate().allow_model_endpoint(&url).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let rec = requests.clone();
        tokio::spawn(async move {
            for r in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let body = read_request(&mut sock).await;
                rec.lock().unwrap().push(body);
                let mut head = format!("HTTP/1.1 {} X\r\nconnection: close\r\n", r.status);
                for (k, v) in &r.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(r.body.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        Server { url, requests }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A1 and A4: adjacent same-role messages merge and whitespace-only text disappears.
    /// A request to a host nobody configured is never sent: the stream ends with an error
    /// that says why, and the fake server sees nothing.
    #[tokio::test]
    async fn an_unconfigured_endpoint_is_refused_before_any_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let c = openai_compat::OpenAiCompatClient::new(url.clone(), None, "m".into()).unwrap();
        let got = collect(c.stream(ChatRequest::default())).await;
        assert_eq!(got.stop, Some(StopReason::Error));
        let msg = got.stop_message.unwrap();
        assert!(
            msg.contains("not a configured model endpoint") && msg.contains("config.toml"),
            "{msg}"
        );
        assert!(c.list_models().await.is_err());
        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "a connection was opened");
        // Naming the endpoint (as the provider configuration does) lets requests through.
        wb_net::gate().allow_model_endpoint(&url).unwrap();
        assert!(wb_net::gate()
            .request(
                wb_net::reqwest::Method::POST,
                &format!("{url}/chat/completions"),
                "chat"
            )
            .is_ok());
    }

    #[test]
    fn normalize_merges_roles_and_drops_blank_text() {
        let msgs = vec![
            Message::user_text("one"),
            Message::user_text("two"),
            Message::assistant_text("  \n "),
            Message::user_text("three"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentPart::Text { text: "\n".into() },
                    ContentPart::ToolCall {
                        id: "c".into(),
                        name: "t".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
            },
            Message::assistant_text("after"),
        ];
        let n = normalize_messages(&msgs);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].role, Role::User);
        assert_eq!(n[0].text(), "one\ntwo\nthree");
        assert_eq!(n[1].role, Role::Assistant);
        assert_eq!(n[1].content.len(), 2);
        assert!(matches!(n[1].content[0], ContentPart::ToolCall { .. }));
    }
}
