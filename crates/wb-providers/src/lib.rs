// Ported from Personal Analyst `crates/core/src/providers/mod.rs` at commit f229e4d.
// Adapted: a self-contained crate (owns its request, event, and config types); `client_for`
// takes the provider entry plus key store, audit sink, and capability cache;
// `ModelClient` gains optional capability hooks; `collect` gathers citations.
//! Shared-to-be crate `wb-providers` (BUILD-SPEC section 4): one `ModelClient` trait, two
//! wire adapters (OpenAI-compatible chat completions and Anthropic Messages), SSE parsing,
//! capability probing, the text-protocol fallback, and gateway support: configurable auth,
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
pub mod text_protocol;
pub mod wire;

pub use audit::{AuditRecord, AuditSink, AuditStatus};
pub use config::{AuthKind, Location, ModelConfig, ProviderConfig, ProviderKind, ToolSupport};
pub use keys::{KeyError, KeyRef, KeyStores};
pub use model::{
    ChatRequest, ContentPart, Message, ModelCapabilities, ModelEvent, Role, StopReason, ToolDef,
};
pub use wire::{CapabilityCacheRef, EndpointConfig, WireFeatures};

use futures::stream::BoxStream;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

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
        .map_err(|e| anyhow::anyhow!("provider `{}`: invalid base_url: {e}", provider.name))?;
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
