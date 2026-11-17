//! Shared HTTP machinery for both wire adapters: auth and extra headers, the unknown-field
//! downgrade, the SSE pump, and the audit hook (Present PROTOCOL 6.1, 10).
//! The pump is factored out of the two `stream` implementations in Personal Analyst
//! `crates/core/src/providers/{anthropic,openai_compat}.rs` at commit f229e4d, which were
//! identical apart from the decoder.

use crate::audit::{AuditRecord, AuditSink, AuditStatus};
use crate::config::{AuthKind, Location, ProviderConfig};
use crate::model::{ChatRequest, ModelCapabilities, ModelEvent, StopReason};
use crate::sse::{SseEvent, SseParser};
use anyhow::{anyhow, Context, Result};
use futures::stream::BoxStream;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use wb_net::reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use wb_net::reqwest::{self, Method, RequestBuilder};

/// Wire names of the optional request fields subject to the unknown-field downgrade.
pub const FIELD_STREAM_OPTIONS: &str = "stream_options";
pub const FIELD_CACHE_CONTROL: &str = "cache_control";
pub const FIELD_CITATIONS: &str = "citations";
/// Document content blocks.
pub const FIELD_DOCUMENT: &str = "document";

/// What an adapter's wire format can express (independent of what a given endpoint accepts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WireFeatures {
    pub documents: bool,
    pub caching: bool,
}

/// The slice of the capability record that shapes request bodies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features {
    pub caching: bool,
    pub documents: bool,
    pub unsupported: BTreeSet<String>,
}

impl Features {
    pub fn from_capabilities(caps: &ModelCapabilities) -> Self {
        Features {
            caching: caps.caching,
            documents: caps.documents,
            unsupported: caps.unsupported_fields.iter().cloned().collect(),
        }
    }

    pub fn allows(&self, field: &str) -> bool {
        !self.unsupported.contains(field)
    }
}

/// Where downgrades are persisted: the capability cache under `home`, record `key`
/// (`provider/model`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityCacheRef {
    pub home: PathBuf,
    pub key: String,
}

/// Live feature state of one client: read for every request, updated by downgrades.
#[derive(Debug, Default)]
pub(crate) struct FeatureState {
    inner: Mutex<Features>,
    persist: Option<CapabilityCacheRef>,
}

impl FeatureState {
    pub fn new(initial: Features, persist: Option<CapabilityCacheRef>) -> Self {
        FeatureState {
            inner: Mutex::new(initial),
            persist,
        }
    }

    pub fn snapshot(&self) -> Features {
        self.inner.lock().map(|f| f.clone()).unwrap_or_default()
    }

    pub fn replace(&self, features: Features) {
        if let Ok(mut f) = self.inner.lock() {
            *f = features;
        }
    }

    /// Record a rejected field in memory and in the persisted cache.
    pub fn mark_unsupported(&self, field: &str) {
        if let Ok(mut f) = self.inner.lock() {
            f.unsupported.insert(field.to_string());
        }
        if let Some(p) = &self.persist {
            if let Err(e) = crate::capabilities::record_unsupported(&p.home, &p.key, field) {
                tracing::warn!(error = %e, field, "could not persist capability downgrade");
            }
        }
    }
}

/// Everything needed to talk to one model on one endpoint.
pub struct EndpointConfig {
    /// Provider name (for the audit record).
    pub provider: String,
    pub location: Location,
    pub base_url: String,
    pub auth: AuthKind,
    pub key: Option<String>,
    /// Extra headers sent on every request; they override the adapter's own headers of the
    /// same name.
    pub headers: BTreeMap<String, String>,
    pub model: String,
    pub audit: Option<Arc<dyn AuditSink>>,
    /// The cached capability record, when there is one. Without it requests carry no
    /// caching markers and no document blocks.
    pub capabilities: Option<ModelCapabilities>,
    /// Where to persist unknown-field downgrades.
    pub capability_cache: Option<CapabilityCacheRef>,
}

impl EndpointConfig {
    /// Endpoint settings from a provider entry, with the key already resolved. This does not
    /// allow the endpoint at the network gate; `client_for` does.
    pub fn from_provider(
        provider: &ProviderConfig,
        key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        EndpointConfig {
            provider: provider.name.clone(),
            location: provider.location,
            base_url: provider.effective_base_url(),
            auth: provider.effective_auth(),
            key,
            headers: provider.headers.clone(),
            model: model.into(),
            audit: None,
            capabilities: None,
            capability_cache: None,
        }
    }
}

pub(crate) struct Endpoint {
    pub provider: String,
    pub location: Location,
    pub base_url: String,
    pub host: String,
    pub model: String,
    pub features: FeatureState,
    auth: Option<(HeaderName, HeaderValue)>,
    extra: HeaderMap,
    audit: Option<Arc<dyn AuditSink>>,
}

impl Endpoint {
    pub fn new(cfg: EndpointConfig) -> Result<Self> {
        let base_url = cfg.base_url.trim_end_matches('/').to_string();
        let host = reqwest::Url::parse(&base_url)
            .with_context(|| format!("provider `{}`: invalid base_url", cfg.provider))?
            .host_str()
            .unwrap_or_default()
            .to_string();
        let auth = match (cfg.auth, cfg.key.as_deref()) {
            (AuthKind::None, _) => None,
            (_, None) => {
                return Err(anyhow!(
                    "provider `{}` needs a key for its auth scheme",
                    cfg.provider
                ))
            }
            (AuthKind::XApiKey, Some(k)) => {
                Some((HeaderName::from_static("x-api-key"), secret_value(k)?))
            }
            (AuthKind::Bearer, Some(k)) => {
                Some((AUTHORIZATION, secret_value(&format!("Bearer {k}"))?))
            }
        };
        let mut extra = HeaderMap::new();
        for (name, value) in &cfg.headers {
            let n = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                anyhow!("provider `{}`: invalid header name `{name}`", cfg.provider)
            })?;
            let v = HeaderValue::from_str(value).map_err(|_| {
                anyhow!(
                    "provider `{}`: invalid value for header `{name}`",
                    cfg.provider
                )
            })?;
            extra.insert(n, v);
        }
        let features = cfg
            .capabilities
            .as_ref()
            .map(Features::from_capabilities)
            .unwrap_or_default();
        Ok(Endpoint {
            provider: cfg.provider,
            location: cfg.location,
            base_url,
            host,
            model: cfg.model,
            features: FeatureState::new(features, cfg.capability_cache),
            auth,
            extra,
            audit: cfg.audit,
        })
    }

    /// Headers for one request: the adapter's fixed headers, then auth, then the configured
    /// extra headers (which win on a name clash).
    pub fn headers(&self, fixed: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (n, v) in fixed {
            h.insert(HeaderName::from_static(n), HeaderValue::from_static(v));
        }
        if let Some((n, v)) = &self.auth {
            h.insert(n.clone(), v.clone());
        }
        for (n, v) in &self.extra {
            h.insert(n.clone(), v.clone());
        }
        h
    }

    /// A request to this endpoint, built by the network gate: it is refused unless the
    /// URL's host and port are an allowed model endpoint. `purpose` is a fixed label for the
    /// egress ledger and never carries request content.
    pub fn request(
        &self,
        method: Method,
        url: &str,
        purpose: &'static str,
        fixed: &[(&'static str, &'static str)],
    ) -> Result<RequestBuilder> {
        let builder = wb_net::gate()
            .request(method, url, purpose)
            .map_err(|e| egress_error(&self.provider, &e))?;
        Ok(builder.headers(self.headers(fixed)))
    }

    /// `GET url` with the endpoint's headers, parsed as JSON.
    pub async fn get_json(
        &self,
        url: String,
        purpose: &'static str,
        fixed: &[(&'static str, &'static str)],
    ) -> Result<Value> {
        Ok(self
            .request(Method::GET, &url, purpose, fixed)?
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

/// A refusal by the network gate, worded for the user.
pub(crate) fn egress_error(provider: &str, e: &wb_net::EgressError) -> anyhow::Error {
    let who = if provider.is_empty() {
        "the request".to_string()
    } else {
        format!("the request to provider `{provider}`")
    };
    anyhow!("{who} was not sent: {e}")
}

/// The ledger label of a streaming request. Probe requests are named after what they probe;
/// everything else is `chat`. The request's audit tag is never copied into the ledger.
pub(crate) fn purpose_of(req: &ChatRequest) -> &'static str {
    match req.audit_tag.as_deref() {
        Some("probe:vision") => "vision-probe",
        Some("probe:documents") => "document-probe",
        Some("probe:tools") => "tool-probe",
        Some(t) if t.starts_with("probe") => "probe",
        _ => "chat",
    }
}

fn secret_value(s: &str) -> Result<HeaderValue> {
    let mut v = HeaderValue::from_str(s)
        .map_err(|_| anyhow!("the API key contains characters not allowed in a header"))?;
    v.set_sensitive(true);
    Ok(v)
}

/// One optional request field: how to tell it was sent and that an error names it.
pub(crate) struct OptionalField {
    pub name: &'static str,
    /// Whether the built body contains the field.
    pub sent: fn(&Value) -> bool,
    /// Whether a (lower-cased) 400 response body names the field.
    pub named: fn(&str) -> bool,
}

/// Turns SSE events into model events for one response.
pub(crate) trait Decoder: Default + Send + 'static {
    fn feed(&mut self, ev: &SseEvent) -> Vec<ModelEvent>;
    fn finish(&mut self) -> Vec<ModelEvent>;
    /// The endpoint declined to answer.
    fn refused(&self) -> bool;
    /// Provider error type of an in-stream error, when one was given.
    fn error_type(&self) -> Option<String>;
}

pub(crate) struct WireSpec {
    pub url: String,
    pub fixed_headers: &'static [(&'static str, &'static str)],
    pub build: fn(&ChatRequest, &Features) -> Value,
    pub optional: &'static [OptionalField],
}

/// Optional fields that were sent in `body` and are named by the 400 response `text`.
pub(crate) fn rejected_fields(
    optional: &'static [OptionalField],
    body: &Value,
    text: &str,
) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    let mut rejected: Vec<&'static str> = optional
        .iter()
        .filter(|f| (f.sent)(body) && (f.named)(&lower))
        .map(|f| f.name)
        .collect();
    // An error about `citations` on a document block names both; drop the narrower field
    // and keep the documents.
    if rejected.contains(&FIELD_CITATIONS) {
        rejected.retain(|f| *f != FIELD_DOCUMENT);
    }
    rejected
}

/// Whether any JSON object in `v` has the key `key`.
pub(crate) fn has_key(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(m) => m.contains_key(key) || m.values().any(|x| has_key(x, key)),
        Value::Array(a) => a.iter().any(|x| has_key(x, key)),
        _ => false,
    }
}

/// A classification safe for the audit log: provider error types are identifiers; anything
/// else collapses to `provider_error`.
fn audit_code(kind: Option<String>) -> String {
    match kind {
        Some(k)
            if !k.is_empty()
                && k.len() <= 64
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) =>
        {
            k
        }
        _ => "provider_error".to_string(),
    }
}

/// `e` with its source chain (reqwest's own Display stops at "error sending request").
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Forwards events to the consumer, holding back the final `Stop` until the audit record
/// has been written, and tallying usage for it.
struct Outlet {
    tx: tokio::sync::mpsc::Sender<ModelEvent>,
    input_tokens: u64,
    output_tokens: u64,
    stop: Option<ModelEvent>,
    closed: bool,
}

impl Outlet {
    async fn emit(&mut self, ev: ModelEvent) {
        match ev {
            ModelEvent::Stop { .. } => {
                if self.stop.is_none() {
                    self.stop = Some(ev);
                }
            }
            other => {
                if let ModelEvent::Usage {
                    input_tokens,
                    output_tokens,
                } = &other
                {
                    self.input_tokens += input_tokens;
                    self.output_tokens += output_tokens;
                }
                if !self.closed && self.tx.send(other).await.is_err() {
                    self.closed = true;
                }
            }
        }
    }

    async fn fail(&mut self, message: String) {
        self.emit(ModelEvent::Stop {
            reason: StopReason::Error,
            message: Some(message),
        })
        .await;
    }
}

/// Run one streaming request: build, send (with at most one downgrade retry), decode, audit.
pub(crate) fn run_stream<D: Decoder>(
    ep: Arc<Endpoint>,
    spec: WireSpec,
    req: ChatRequest,
) -> BoxStream<'static, ModelEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut out = Outlet {
            tx,
            input_tokens: 0,
            output_tokens: 0,
            stop: None,
            closed: false,
        };
        let (mut status, mut error) = drive::<D>(&ep, &spec, &req, &mut out).await;
        if out.closed && out.stop.is_none() {
            status = AuditStatus::Error;
            error = Some("cancelled".to_string());
        }
        if let Some(sink) = &ep.audit {
            sink.record(&AuditRecord {
                ts: crate::capabilities::now_rfc3339(),
                provider: ep.provider.clone(),
                location: ep.location,
                base_url_host: ep.host.clone(),
                model: ep.model.clone(),
                input_tokens: out.input_tokens,
                output_tokens: out.output_tokens,
                status,
                error,
                tag: req.audit_tag.clone(),
            });
        }
        if let Some(stop) = out.stop.take() {
            let _ = out.tx.send(stop).await;
        }
    });
    crate::channel_stream(rx)
}

async fn drive<D: Decoder>(
    ep: &Endpoint,
    spec: &WireSpec,
    req: &ChatRequest,
    out: &mut Outlet,
) -> (AuditStatus, Option<String>) {
    let mut body = (spec.build)(req, &ep.features.snapshot());
    let mut retried = false;
    let resp = loop {
        let builder = match ep.request(Method::POST, &spec.url, purpose_of(req), spec.fixed_headers)
        {
            Ok(b) => b,
            Err(e) => {
                out.fail(e.to_string()).await;
                return (AuditStatus::Error, Some("egress_denied".into()));
            }
        };
        let sent = builder.json(&body).send().await;
        let resp = match sent {
            Ok(r) => r,
            Err(e) => {
                out.fail(format!("request failed: {}", error_chain(&e)))
                    .await;
                return (AuditStatus::Error, Some("transport".into()));
            }
        };
        let status = resp.status();
        if status.is_success() {
            break resp;
        }
        let text = resp.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::BAD_REQUEST && !retried {
            let rejected = rejected_fields(spec.optional, &body, &text);
            if !rejected.is_empty() {
                for field in &rejected {
                    tracing::warn!(
                        provider = %ep.provider,
                        model = %ep.model,
                        field,
                        "endpoint rejected an optional request field; retrying without it"
                    );
                    ep.features.mark_unsupported(field);
                }
                body = (spec.build)(req, &ep.features.snapshot());
                retried = true;
                continue;
            }
        }
        out.fail(format!("HTTP {status}: {}", truncate_bytes(&text, 2000)))
            .await;
        return (
            AuditStatus::Error,
            Some(format!("http_{}", status.as_u16())),
        );
    };

    use futures::StreamExt;
    let mut bytes = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut state = D::default();
    while let Some(chunk) = bytes.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                out.fail(format!("stream error: {}", error_chain(&e))).await;
                return (AuditStatus::Error, Some("stream".into()));
            }
        };
        for ev in parser.push(&chunk) {
            for me in state.feed(&ev) {
                out.emit(me).await;
            }
        }
        if out.closed {
            return (AuditStatus::Error, Some("cancelled".into()));
        }
    }
    if let Some(ev) = parser.finish() {
        for me in state.feed(&ev) {
            out.emit(me).await;
        }
    }
    for me in state.finish() {
        out.emit(me).await;
    }
    match &out.stop {
        Some(ModelEvent::Stop {
            reason: StopReason::Error,
            ..
        }) if !state.refused() => (AuditStatus::Error, Some(audit_code(state.error_type()))),
        _ if state.refused() => (AuditStatus::Refused, None),
        _ => (AuditStatus::Ok, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(auth: AuthKind, key: Option<&str>) -> EndpointConfig {
        EndpointConfig {
            provider: "p".into(),
            location: Location::Internal,
            base_url: "https://gw.corp.example/api/".into(),
            auth,
            key: key.map(str::to_string),
            headers: BTreeMap::from([("X-Team".to_string(), "strategy".to_string())]),
            model: "m".into(),
            audit: None,
            capabilities: None,
            capability_cache: None,
        }
    }

    #[test]
    fn header_construction_per_auth_kind() {
        let fixed: &[(&str, &str)] = &[("anthropic-version", "2023-06-01")];
        let ep = Endpoint::new(cfg(AuthKind::XApiKey, Some("k1"))).unwrap();
        let h = ep.headers(fixed);
        assert_eq!(h["x-api-key"], "k1");
        assert!(h.get("authorization").is_none());
        assert_eq!(h["x-team"], "strategy");
        assert_eq!(h["anthropic-version"], "2023-06-01");
        assert_eq!(ep.host, "gw.corp.example");
        assert_eq!(ep.base_url, "https://gw.corp.example/api");

        let ep = Endpoint::new(cfg(AuthKind::Bearer, Some("k1"))).unwrap();
        let h = ep.headers(fixed);
        assert_eq!(h["authorization"], "Bearer k1");
        assert!(h["authorization"].is_sensitive());
        assert!(h.get("x-api-key").is_none());

        let ep = Endpoint::new(cfg(AuthKind::None, Some("k1"))).unwrap();
        let h = ep.headers(fixed);
        assert!(h.get("authorization").is_none() && h.get("x-api-key").is_none());

        assert!(Endpoint::new(cfg(AuthKind::Bearer, None)).is_err());
    }

    #[test]
    fn extra_headers_override_fixed_and_bad_ones_are_rejected() {
        let mut c = cfg(AuthKind::None, None);
        c.headers
            .insert("anthropic-version".into(), "2099-01-01".into());
        let ep = Endpoint::new(c).unwrap();
        let h = ep.headers(&[("anthropic-version", "2023-06-01")]);
        assert_eq!(h["anthropic-version"], "2099-01-01");
        assert_eq!(h.get_all("anthropic-version").iter().count(), 1);

        let mut c = cfg(AuthKind::None, None);
        c.headers.insert("bad name".into(), "x".into());
        assert!(Endpoint::new(c).is_err());
        let mut c = cfg(AuthKind::None, None);
        c.headers.insert("x-ok".into(), "line\nbreak".into());
        assert!(Endpoint::new(c).is_err());
    }

    #[test]
    fn ledger_purposes_are_fixed_labels() {
        let mut req = ChatRequest::default();
        assert_eq!(purpose_of(&req), "chat");
        req.audit_tag = Some("conv-1:7 the user said: secret".into());
        assert_eq!(purpose_of(&req), "chat");
        req.audit_tag = Some("probe:vision".into());
        assert_eq!(purpose_of(&req), "vision-probe");
        req.audit_tag = Some("probe:tools".into());
        assert_eq!(purpose_of(&req), "tool-probe");
    }

    #[test]
    fn audit_codes_never_carry_free_text() {
        assert_eq!(
            audit_code(Some("overloaded_error".into())),
            "overloaded_error"
        );
        assert_eq!(
            audit_code(Some("the user said: secret".into())),
            "provider_error"
        );
        assert_eq!(audit_code(None), "provider_error");
    }

    #[test]
    fn has_key_searches_nested_values() {
        let v = json!({"a": [{"b": {"cache_control": 1}}]});
        assert!(has_key(&v, "cache_control"));
        assert!(!has_key(&v, "citations"));
    }
}
