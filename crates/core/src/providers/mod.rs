//! Model provider layer (PROTOCOL 4, ARCHITECTURE 5.10): one `ModelClient` trait, two wire
//! adapters, SSE parsing, capability probing, and the text-protocol fallback.

pub mod anthropic;
pub mod capabilities;
pub mod names;
pub mod openai_compat;
pub mod sse;
pub mod text_protocol;

use crate::config::ResolvedModel;
use analyst_protocol::config::ProviderKind;
use analyst_protocol::model::{ChatRequest, ContentPart, Message, ModelEvent, StopReason};
use futures::stream::BoxStream;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wb_net::{reqwest, EgressError};

/// A streaming chat client. Every adapter produces exactly the `ModelEvent` sequence of
/// PROTOCOL 4 and always terminates the stream with a `Stop` event.
pub trait ModelClient: Send + Sync {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ModelEvent>;
    /// Model id to put in requests and usage events.
    fn model_id(&self) -> &str;
}

/// Build the client for a resolved model (keys from the environment or the default home's
/// `.env`).
pub fn client_for(resolved: &ResolvedModel) -> anyhow::Result<Arc<dyn ModelClient>> {
    client_for_in(&crate::config::analyst_home(), resolved)
}

/// [`client_for`] with an explicit analyst home for stored keys.
pub fn client_for_in(
    home: &Path,
    resolved: &ResolvedModel,
) -> anyhow::Result<Arc<dyn ModelClient>> {
    client_for_repo(home, None, resolved)
}

/// [`client_for_in`] for a session: the repo's own untracked `.env` is consulted for the key
/// after the process environment and `<home>/.env`.
pub fn client_for_repo(
    home: &Path,
    repo: Option<&Path>,
    resolved: &ResolvedModel,
) -> anyhow::Result<Arc<dyn ModelClient>> {
    let api_key = crate::config::api_key_in(home, repo, &resolved.provider)?;
    let base_url = resolved.provider.effective_base_url();
    // The user's provider configuration is what puts a host on the gate's allowlist.
    wb_net::gate()
        .allow_model_endpoint(&base_url)
        .map_err(|e| {
            anyhow::anyhow!(
                "provider `{}`: {}",
                resolved.provider.name,
                egress_message(&e)
            )
        })?;
    Ok(match resolved.provider.kind {
        ProviderKind::OpenAiCompatible => Arc::new(openai_compat::OpenAiCompatClient::new(
            base_url,
            api_key,
            resolved.spec.id.clone(),
        )?),
        ProviderKind::Anthropic => Arc::new(anthropic::AnthropicClient::new(
            base_url,
            api_key.ok_or_else(|| {
                anyhow::anyhow!(
                    "provider `{}` (anthropic) needs an API key: set `api_key_env` in config.toml or store a key",
                    resolved.provider.name
                )
            })?,
            resolved.spec.id.clone(),
        )?),
    })
}

/// The model's context window as reported by the provider, when it reports one: Anthropic
/// `GET /v1/models/{id}` (`max_input_tokens`), OpenAI-compatible `GET /models`
/// (`context_length`, `context_window`, `max_model_len`, ...). `None` on any failure.
pub async fn model_info_window(
    home: &Path,
    repo: Option<&Path>,
    resolved: &ResolvedModel,
) -> Option<u32> {
    let key = crate::config::api_key_in(home, repo, &resolved.provider)
        .ok()
        .flatten();
    let base = resolved.provider.effective_base_url();
    wb_net::gate().allow_model_endpoint(&base).ok()?;
    let id = resolved.spec.id.clone();
    let info = match resolved.provider.kind {
        ProviderKind::OpenAiCompatible => {
            let c = openai_compat::OpenAiCompatClient::new(base, key, id.clone()).ok()?;
            tokio::time::timeout(Duration::from_secs(10), c.list_models())
                .await
                .ok()?
                .ok()?
        }
        ProviderKind::Anthropic => {
            let c = anthropic::AnthropicClient::new(base, key?, id.clone()).ok()?;
            tokio::time::timeout(Duration::from_secs(10), c.model_info())
                .await
                .ok()?
                .ok()?
        }
    };
    capabilities::context_window_from_info(&info, &id)
}

/// Turn a receiver of events into a boxed stream (adapters push from a spawned task).
pub(crate) fn channel_stream(
    rx: tokio::sync::mpsc::Receiver<ModelEvent>,
) -> BoxStream<'static, ModelEvent> {
    Box::pin(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|e| (e, rx))
    }))
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

/// Timeouts and retry policy for provider HTTP calls (PROTOCOL 9).
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

/// Build a request to a model endpoint. This is the only way the provider layer reaches the
/// network: the gate refuses (and records) any destination that is not a configured
/// provider `base_url`. `purpose` is a ledger label (`chat`, `probe`), never request content.
pub(crate) fn gated(
    method: reqwest::Method,
    url: &str,
    purpose: &str,
) -> Result<reqwest::RequestBuilder, EgressError> {
    wb_net::gate().request(method, url, purpose)
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
            "the request was not sent: the network ledger (egress.jsonl in the analyst home) could not be written ({why}). Nothing leaves the machine unrecorded; make the home directory writable"
        ),
        EgressError::Client(why) => format!("the HTTP client could not be built: {why}"),
    }
}

/// Why a request produced no usable response.
#[derive(Debug)]
pub(crate) enum SendFailure {
    Http {
        status: u16,
        body: String,
    },
    Transport(String),
    /// The network gate refused the destination; never retried.
    Egress(String),
}

impl SendFailure {
    pub(crate) fn message(&self) -> String {
        match self {
            SendFailure::Http { status, body } => {
                format!("HTTP {status}: {}", crate::util::truncate_bytes(body, 2000))
            }
            SendFailure::Transport(e) => format!("request failed: {e}"),
            SendFailure::Egress(e) => e.clone(),
        }
    }
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || (500..=599).contains(&status)
}

/// Send a request, retrying with exponential backoff on connection errors, 429, 5xx, and
/// overload (529) responses. Nothing has been streamed to the caller at this point, so a
/// retry cannot duplicate output. `Retry-After` (seconds) is honoured up to 30 s.
pub(crate) async fn send_with_retry<F>(
    make: F,
    policy: &HttpPolicy,
) -> Result<reqwest::Response, SendFailure>
where
    F: Fn() -> Result<reqwest::RequestBuilder, EgressError>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let request = make().map_err(|e| SendFailure::Egress(egress_message(&e)))?;
        let sent = tokio::time::timeout(policy.first_byte_timeout, request.send()).await;
        let (failure, retry_after) = match sent {
            Ok(Ok(resp)) if resp.status().is_success() => return Ok(resp),
            Ok(Ok(resp)) => {
                let status = resp.status().as_u16();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok());
                let body = resp.text().await.unwrap_or_default();
                let failure = SendFailure::Http { status, body };
                if !retryable_status(status) {
                    return Err(failure);
                }
                (failure, retry_after)
            }
            Ok(Err(e)) => (SendFailure::Transport(e.to_string()), None),
            Err(_) => (
                SendFailure::Transport(format!(
                    "no response within {} s",
                    policy.first_byte_timeout.as_secs()
                )),
                None,
            ),
        };
        if attempt >= policy.max_attempts.max(1) {
            return Err(failure);
        }
        let backoff = policy.backoff_base * 2u32.saturating_pow(attempt - 1);
        let delay = match retry_after {
            Some(s) => Duration::from_secs(s.min(30)).max(backoff),
            None => backoff,
        };
        tracing::warn!(
            "model request failed ({}); retry {attempt}/{} in {:?}",
            failure.message(),
            policy.max_attempts - 1,
            delay
        );
        tokio::time::sleep(delay).await;
    }
}

/// Per-response state of a wire adapter, driven by [`pump`].
pub(crate) trait SseState: Send {
    fn on_event(&mut self, ev: &sse::SseEvent) -> Vec<ModelEvent>;
    /// The body ended. Must yield `Stop { reason: Error }` when no terminal event was seen.
    fn on_eof(&mut self) -> Vec<ModelEvent>;
}

async fn stop_error(tx: &tokio::sync::mpsc::Sender<ModelEvent>, message: String) {
    let _ = tx
        .send(ModelEvent::Stop {
            reason: StopReason::Error,
            message: Some(message),
        })
        .await;
}

/// Read an SSE response body into model events, with an idle timeout between chunks.
pub(crate) async fn pump(
    resp: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<ModelEvent>,
    idle_timeout: Duration,
    mut state: impl SseState,
) {
    use futures::StreamExt;
    let mut bytes = resp.bytes_stream();
    let mut parser = sse::SseParser::new();
    loop {
        let chunk = match tokio::time::timeout(idle_timeout, bytes.next()).await {
            Err(_) => {
                stop_error(
                    &tx,
                    format!(
                        "the model stream stalled: no data for {} s",
                        idle_timeout.as_secs()
                    ),
                )
                .await;
                return;
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                stop_error(&tx, format!("stream error: {e}")).await;
                return;
            }
            Ok(Some(Ok(c))) => c,
        };
        for ev in parser.push(&chunk) {
            for me in state.on_event(&ev) {
                if tx.send(me).await.is_err() {
                    return;
                }
            }
        }
    }
    if let Some(ev) = parser.finish() {
        for me in state.on_event(&ev) {
            let _ = tx.send(me).await;
        }
    }
    for me in state.on_eof() {
        let _ = tx.send(me).await;
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
    let mut v = serde_json::json!({ "_raw": crate::util::truncate_bytes(raw, 2000) });
    if truncated {
        v["_truncated"] = serde_json::json!(true);
    }
    v
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

/// Collect a full response from a stream: text, completed tool calls, usage, stop reason.
#[derive(Debug, Default, Clone)]
pub struct Collected {
    pub text: String,
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
    use analyst_protocol::model::Role;

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
        assert!(gated(
            reqwest::Method::POST,
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
