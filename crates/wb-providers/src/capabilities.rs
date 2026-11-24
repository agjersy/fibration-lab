// Ported from Personal Analyst `crates/core/src/providers/capabilities.rs` at commit f229e4d.
// Adapted (Present PROTOCOL 6.1): adds the vision probe, the documents and caching probes,
// the `degraded` computation, and persistence of unknown-field downgrades.
//! Capability probe and the `<home>/capabilities.json` cache.

use crate::config::{ModelConfig, ToolSupport};
use crate::model::{
    ChatRequest, ContentPart, Message, ModelCapabilities, Role, StopReason, ToolDef,
};
use crate::probe_media;
use crate::wire::{FIELD_CACHE_CONTROL, FIELD_DOCUMENT};
use crate::{collect, ModelClient};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONTEXT_WINDOW: u32 = 32768;

pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A random number below `bound`, from the standard library's per-process hash seed.
fn random_below(bound: u64) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish() % bound
}

pub fn cache_path(home: &Path) -> PathBuf {
    home.join("capabilities.json")
}

/// Load the cache (missing file = empty).
pub fn load_cache(home: &Path) -> Result<BTreeMap<String, ModelCapabilities>> {
    let path = cache_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save_cache(home: &Path, cache: &BTreeMap<String, ModelCapabilities>) -> Result<()> {
    std::fs::create_dir_all(home)?;
    let path = cache_path(home);
    std::fs::write(&path, serde_json::to_string_pretty(cache)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// Cached record for `provider/model`, if any.
pub fn cached(home: &Path, key: &str) -> Option<ModelCapabilities> {
    load_cache(home).ok()?.remove(key)
}

pub fn store(home: &Path, key: &str, caps: &ModelCapabilities) -> Result<()> {
    let mut cache = load_cache(home)?;
    cache.insert(key.to_string(), caps.clone());
    save_cache(home, &cache)
}

/// Record in the cached record for `key` that the endpoint rejected `field`. When the model
/// has not been probed yet this creates a stub record (empty `probed_at`), which `ensure`
/// does not treat as a probe result.
pub fn record_unsupported(home: &Path, key: &str, field: &str) -> Result<()> {
    let mut cache = load_cache(home)?;
    cache
        .entry(key.to_string())
        .or_default()
        .mark_unsupported(field);
    save_cache(home, &cache)
}

/// The probe tool definition.
pub fn probe_tool() -> ToolDef {
    ToolDef {
        name: "echo_probe".into(),
        description:
            "Echo the given token back. Call this tool exactly once with the token you were given."
                .into(),
        parameters: json!({
            "type": "object",
            "properties": {"token": {"type": "string"}},
            "required": ["token"]
        }),
    }
}

/// Whether `text` contains `number` as a whole run of digits.
fn mentions_number(text: &str, number: &str) -> bool {
    text.split(|c: char| !c.is_ascii_digit())
        .any(|run| run == number)
}

/// Vision probe: a 64x64 PNG with a two-digit number the model must read back.
pub async fn probe_vision(client: &dyn ModelClient) -> bool {
    let number = 10 + random_below(90) as u8;
    let png = probe_media::vision_probe_png(number);
    let req = ChatRequest {
        model: client.model_id().to_string(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: vec![
                ContentPart::Image {
                    media_type: "image/png".into(),
                    data: base64::engine::general_purpose::STANDARD.encode(png),
                },
                ContentPart::Text {
                    text: "The image shows a two-digit number. Reply with only that number.".into(),
                },
            ],
        }],
        tools: vec![],
        max_tokens: Some(200),
        audit_tag: Some("probe:vision".into()),
    };
    let c = collect(client.stream(req)).await;
    c.stop != Some(StopReason::Error) && mentions_number(&c.text, &number.to_string())
}

/// Documents probe: a one-page PDF with a four-digit code the model must read back from a
/// native document block.
pub async fn probe_documents(client: &dyn ModelClient) -> bool {
    let code = (1000 + random_below(9000)).to_string();
    let req = ChatRequest {
        model: client.model_id().to_string(),
        system: None,
        messages: vec![Message {
            role: Role::User,
            content: vec![
                ContentPart::Document {
                    media_type: "application/pdf".into(),
                    data: base64::engine::general_purpose::STANDARD
                        .encode(probe_media::document_probe_pdf(&code)),
                    title: Some("probe".into()),
                },
                ContentPart::Text {
                    text: "The document contains a four-digit code. Reply with only that code."
                        .into(),
                },
            ],
        }],
        tools: vec![],
        max_tokens: Some(200),
        audit_tag: Some("probe:documents".into()),
    };
    let c = collect(client.stream(req)).await;
    c.stop != Some(StopReason::Error)
        && !client
            .unsupported_fields()
            .iter()
            .any(|f| f == FIELD_DOCUMENT)
        && mentions_number(&c.text, &code)
}

/// Apply the `[[models]]` config overrides on top of a capability record. Overrides always
/// win, whether the record was just probed or came from the cache.
pub fn apply_overrides(
    mut caps: ModelCapabilities,
    overrides: Option<&ModelConfig>,
) -> ModelCapabilities {
    if let Some(o) = overrides {
        if let Some(t) = o.tools {
            caps.tools = t;
        }
        if let Some(c) = o.context_window {
            caps.context_window = c;
        }
        if let Some(v) = o.vision {
            caps.vision = v;
        }
        if o.max_output_tokens.is_some() {
            caps.max_output_tokens = o.max_output_tokens;
        }
        caps.recompute_degraded();
    }
    caps
}

/// Why a model without native tool calling cannot be used, in words a user can act on.
pub fn not_native_message(key: &str) -> String {
    format!(
        "model `{key}` does not support native tool calling, which this app requires: it did \
         not call the tool the capability probe asked for. Choose a model with tool calling. \
         If this model does support it, set `tools = \"native\"` for it under [[models]] in \
         config.toml"
    )
}

/// Refuse a capability record without native tool calling.
pub fn require_native_tools(key: &str, caps: &ModelCapabilities) -> Result<()> {
    if caps.tools == ToolSupport::Native {
        Ok(())
    } else {
        Err(anyhow!(not_native_message(key)))
    }
}

/// Probe a model: native tool calling, vision, documents, caching. `context_window` comes
/// from the override or the model-info value when given, else the default. Overrides in the
/// `[[models]]` entry win over probe results (and skip the corresponding request).
///
/// A probe that could not be completed (request or provider error) is an `Err`: it says
/// nothing about the model and must not be recorded. A model that answered without calling
/// the probe tool gets `tools: none`; `ensure` rejects such a record.
///
/// The probe sends every optional request feature the adapter's wire format has; endpoints
/// that reject one trigger the unknown-field downgrade, which is how `caching` and the
/// record's `unsupported_fields` are learned. The final record is applied to the client.
pub async fn probe(
    client: &dyn ModelClient,
    overrides: Option<&ModelConfig>,
    info_context_window: Option<u32>,
) -> Result<ModelCapabilities> {
    let wire = client.wire_features();
    // Start optimistic and with no recorded rejections, so a re-probe notices an endpoint
    // that has since learned a field.
    client.apply_capabilities(&ModelCapabilities {
        documents: wire.documents,
        caching: wire.caching,
        ..ModelCapabilities::default()
    });

    let token = format!("probe-{:06x}", random_below(1 << 24));
    let req = ChatRequest {
        model: client.model_id().to_string(),
        system: Some("You are a tool-calling test harness. Follow the instruction exactly.".into()),
        messages: vec![Message::user_text(format!(
            "Call the echo_probe tool with token \"{token}\". Do not reply with text."
        ))],
        tools: vec![probe_tool()],
        max_tokens: Some(1024),
        audit_tag: Some("probe:tools".into()),
    };
    let c = collect(client.stream(req)).await;
    if c.stop.is_none() || c.stop == Some(StopReason::Error) {
        return Err(anyhow!(
            "capability probe failed: {}",
            c.stop_message.unwrap_or_else(|| "no response".into())
        ));
    }
    let native = c.tool_calls.iter().any(|(_, name, args)| {
        name == "echo_probe" && args.get("token").and_then(Value::as_str) == Some(token.as_str())
    });
    let tools = match overrides.and_then(|o| o.tools) {
        Some(t) => t,
        None if native => ToolSupport::Native,
        None => ToolSupport::None,
    };
    let context_window = overrides
        .and_then(|o| o.context_window)
        .or(info_context_window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let vision = match overrides.and_then(|o| o.vision) {
        Some(v) => v,
        None => probe_vision(client).await,
    };
    let documents = wire.documents && probe_documents(client).await;

    let mut unsupported_fields = client.unsupported_fields();
    unsupported_fields.sort();
    // The tool probe carried the caching markers (system prompt and tool definition).
    let caching = wire.caching && !unsupported_fields.iter().any(|f| f == FIELD_CACHE_CONTROL);
    let mut caps = ModelCapabilities {
        context_window,
        tools,
        vision,
        documents,
        caching,
        unsupported_fields,
        degraded: false,
        degraded_reasons: Vec::new(),
        probed_at: now_rfc3339(),
        max_output_tokens: overrides.and_then(|o| o.max_output_tokens),
    };
    caps.recompute_degraded();
    client.apply_capabilities(&caps);
    Ok(caps)
}

/// Pull a context window out of a model-info JSON document from either provider family
/// (`context_length`, `context_window`, `max_model_len`, `max_input_tokens`, Ollama
/// `model_info.*.context_length`).
pub fn context_window_from_info(info: &Value, model_id: &str) -> Option<u32> {
    fn scan(v: &Value) -> Option<u32> {
        match v {
            Value::Object(m) => {
                for (k, val) in m {
                    if (k == "context_length"
                        || k == "context_window"
                        || k == "max_model_len"
                        || k == "max_context_length"
                        || k == "max_input_tokens"
                        || k.ends_with(".context_length"))
                        && val.is_u64()
                    {
                        return val.as_u64().map(|n| n as u32);
                    }
                }
                m.values().find_map(scan)
            }
            Value::Array(a) => a.iter().find_map(scan),
            _ => None,
        }
    }
    // Prefer the entry matching the model id in a `data` listing.
    if let Some(list) = info.get("data").and_then(Value::as_array) {
        if let Some(entry) = list
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(model_id))
        {
            return scan(entry);
        }
        return None;
    }
    scan(info)
}

/// Capabilities for a model, from cache or by probing. Config overrides are applied on top
/// in both cases, and the result is applied to the client.
///
/// Only a completed probe of a model with native tool calling is cached. A model without it
/// is rejected with an error, and so is a probe that fails (the next call probes again),
/// unless the config already says the model takes tools natively: then an unreachable
/// provider does not block anything and an uncached record built from the overrides is
/// returned.
pub async fn ensure(
    home: &Path,
    key: &str,
    client: &dyn ModelClient,
    overrides: Option<&ModelConfig>,
    info_context_window: Option<u32>,
    force: bool,
) -> Result<ModelCapabilities> {
    if let Some(o) = overrides {
        if o.tools.is_some_and(|t| t != ToolSupport::Native) {
            return Err(anyhow!(not_native_message(key)));
        }
    }
    if !force {
        // A stub (empty `probed_at`) is not a probe result, and a record without native
        // tools (written by an earlier version) is probed again rather than trusted.
        let hit =
            cached(home, key).filter(|c| !c.probed_at.is_empty() && c.tools == ToolSupport::Native);
        if let Some(c) = hit {
            let c = apply_overrides(c, overrides);
            client.apply_capabilities(&c);
            return Ok(c);
        }
    }
    let caps = match probe(client, overrides, info_context_window).await {
        Ok(caps) => caps,
        Err(_) if overrides.is_some_and(|o| o.tools == Some(ToolSupport::Native)) => {
            let caps = apply_overrides(
                ModelCapabilities {
                    context_window: info_context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
                    unsupported_fields: client.unsupported_fields(),
                    ..ModelCapabilities::default()
                },
                overrides,
            );
            client.apply_capabilities(&caps);
            return Ok(caps);
        }
        Err(e) => return Err(e),
    };
    require_native_tools(key, &caps)?;
    store(home, key, &caps)?;
    Ok(caps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelEvent, REASON_CONTEXT, REASON_TOOLS, REASON_VISION};
    use crate::ScriptedModelClient;

    /// Answers each probe the way a capable model would, by inspecting the request.
    struct Echo {
        vision: bool,
    }

    impl ModelClient for Echo {
        fn stream(&self, req: ChatRequest) -> futures::stream::BoxStream<'static, ModelEvent> {
            let image = req.messages[0].content.iter().find_map(|p| match p {
                ContentPart::Image { data, .. } => Some(data.clone()),
                _ => None,
            });
            let events = if let Some(data) = image {
                let b64 = &base64::engine::general_purpose::STANDARD;
                let n = (10..=99u8)
                    .find(|n| b64.encode(probe_media::vision_probe_png(*n)) == data)
                    .expect("probe image is one of the 90 probe PNGs");
                let reply = if self.vision {
                    format!("The number is {n}.")
                } else {
                    "I cannot view images.".to_string()
                };
                vec![ModelEvent::Text { delta: reply }]
            } else {
                let text = req.messages[0].text();
                let token = text.split('"').nth(1).unwrap_or("").to_string();
                vec![
                    ModelEvent::ToolCall {
                        id: "1".into(),
                        name: "echo_probe".into(),
                        arguments: json!({"token": token}),
                    },
                    ModelEvent::Stop {
                        reason: StopReason::ToolCalls,
                        message: None,
                    },
                ]
            };
            Box::pin(
                futures::stream::iter(events).chain(futures::stream::iter(vec![
                    ModelEvent::Stop {
                        reason: StopReason::End,
                        message: None,
                    },
                ])),
            )
        }
        fn model_id(&self) -> &str {
            "echo"
        }
    }
    use futures::StreamExt;

    #[tokio::test]
    async fn probe_detects_native_tools_and_ensure_rejects_the_rest() {
        // The scripted client answers in prose: it never calls the probe tool.
        let prose = ScriptedModelClient::new(vec![]);
        let caps = probe(&prose, None, None).await.unwrap();
        assert_eq!(caps.tools, ToolSupport::None);
        assert_eq!(caps.context_window, DEFAULT_CONTEXT_WINDOW);
        assert!(!caps.vision && !caps.documents && !caps.caching);
        assert_eq!(
            caps.degraded_reasons,
            vec![REASON_TOOLS, REASON_CONTEXT, REASON_VISION]
        );
        assert_eq!(
            prose.calls(),
            2,
            "tool probe and vision probe; no document probe"
        );

        // `ensure` refuses such a model with an explanation and remembers nothing.
        let dir = tempfile::tempdir().unwrap();
        let err = ensure(dir.path(), "p/prose", &prose, None, None, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("p/prose") && err.contains("native tool calling"),
            "{err}"
        );
        assert!(cached(dir.path(), "p/prose").is_none());

        let caps = probe(&Echo { vision: false }, None, Some(65536))
            .await
            .unwrap();
        assert_eq!(caps.tools, ToolSupport::Native);
        assert_eq!(caps.context_window, 65536);
        assert!(!caps.vision);
        assert_eq!(caps.degraded_reasons, vec![REASON_CONTEXT, REASON_VISION]);

        store(dir.path(), "p/m", &caps).unwrap();
        let back = cached(dir.path(), "p/m").unwrap();
        assert_eq!(back, caps);
        let ensured = ensure(dir.path(), "p/m", &prose, None, None, false)
            .await
            .unwrap();
        assert_eq!(ensured.tools, ToolSupport::Native, "cache wins");
    }

    /// Records written before the text-protocol tier was removed still load; they are not
    /// trusted, so the model is probed again.
    #[tokio::test]
    async fn legacy_text_protocol_records_load_and_are_probed_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            cache_path(dir.path()),
            r#"{"p/m": {"context_window": 32768, "tools": "text-protocol", "vision": false, "probed_at": "2026-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert_eq!(cached(dir.path(), "p/m").unwrap().tools, ToolSupport::None);
        let caps = ensure(
            dir.path(),
            "p/m",
            &Echo { vision: false },
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(caps.tools, ToolSupport::Native);
        assert_eq!(
            cached(dir.path(), "p/m").unwrap().tools,
            ToolSupport::Native
        );
        // A `[[models]]` entry that still says `text-protocol` is refused before any request.
        let o: ModelConfig =
            serde_json::from_value(json!({"provider": "p", "id": "x", "tools": "text-protocol"}))
                .unwrap();
        let client = ScriptedModelClient::new(vec![]);
        let err = ensure(dir.path(), "p/x", &client, Some(&o), None, false).await;
        assert!(err.unwrap_err().to_string().contains("native tool calling"));
        assert_eq!(client.calls(), 0);
    }

    struct Failing;
    impl ModelClient for Failing {
        fn stream(&self, req: ChatRequest) -> futures::stream::BoxStream<'static, ModelEvent> {
            assert!(
                serde_json::to_value(&req)
                    .unwrap()
                    .get("temperature")
                    .is_none(),
                "probes send no temperature"
            );
            Box::pin(futures::stream::iter(vec![ModelEvent::Stop {
                reason: StopReason::Error,
                message: Some("HTTP 529: overloaded".into()),
            }]))
        }
        fn model_id(&self) -> &str {
            "failing"
        }
    }

    /// B5: a failed probe is not remembered, and overrides beat the cache.
    #[tokio::test]
    async fn failed_probe_is_not_cached_and_overrides_always_apply() {
        let dir = tempfile::tempdir().unwrap();
        let err = ensure(dir.path(), "p/m", &Failing, None, None, false).await;
        assert!(err.unwrap_err().to_string().contains("overloaded"));
        assert!(
            cached(dir.path(), "p/m").is_none(),
            "failure must not be cached"
        );
        // The next attempt probes again and succeeds.
        let ok = ensure(
            dir.path(),
            "p/m",
            &Echo { vision: false },
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(ok.tools, ToolSupport::Native);
        assert!(!ok.vision);
        // An override wins over the cached record without re-probing.
        let o = ModelConfig {
            provider: "p".into(),
            id: "m".into(),
            context_window: Some(200_000),
            tools: Some(ToolSupport::Native),
            vision: Some(true),
            max_output_tokens: Some(8000),
        };
        let with = ensure(dir.path(), "p/m", &Failing, Some(&o), None, false)
            .await
            .unwrap();
        assert_eq!(with.context_window, 200_000);
        assert!(with.vision && !with.degraded);
        assert_eq!(with.max_output_tokens, Some(8000));
        let stored = cached(dir.path(), "p/m").unwrap();
        assert_eq!(stored.context_window, DEFAULT_CONTEXT_WINDOW);
        assert!(!stored.vision);
        // With `tools` configured, an unreachable provider does not block anything, and
        // nothing is cached from the failure.
        let fresh = tempfile::tempdir().unwrap();
        let caps = ensure(fresh.path(), "p/m", &Failing, Some(&o), None, false)
            .await
            .unwrap();
        assert_eq!(caps.tools, ToolSupport::Native);
        assert_eq!(caps.context_window, 200_000);
        assert!(cached(fresh.path(), "p/m").is_none());
    }

    #[tokio::test]
    async fn baseline_model_is_not_degraded() {
        let caps = probe(&Echo { vision: true }, None, Some(200_000))
            .await
            .unwrap();
        assert!(caps.vision);
        assert_eq!(caps.tools, ToolSupport::Native);
        assert!(!caps.degraded, "{:?}", caps.degraded_reasons);
        assert!(caps.degraded_reasons.is_empty());
        assert!(!caps.probed_at.is_empty());
    }

    #[tokio::test]
    async fn overrides_win_and_skip_the_vision_request() {
        let client = ScriptedModelClient::new(vec![]);
        let overrides = ModelConfig {
            provider: "p".into(),
            id: "m".into(),
            context_window: Some(1_000_000),
            tools: Some(ToolSupport::Native),
            vision: Some(true),
            max_output_tokens: None,
        };
        let caps = probe(&client, Some(&overrides), Some(4096)).await.unwrap();
        assert_eq!(client.calls(), 1);
        assert_eq!(caps.context_window, 1_000_000);
        assert!(caps.vision && !caps.degraded);
    }

    #[tokio::test]
    async fn stub_records_from_downgrades_do_not_count_as_probed() {
        let dir = tempfile::tempdir().unwrap();
        record_unsupported(dir.path(), "p/m", "cache_control").unwrap();
        record_unsupported(dir.path(), "p/m", "stream_options").unwrap();
        let stub = cached(dir.path(), "p/m").unwrap();
        assert_eq!(
            stub.unsupported_fields,
            vec!["cache_control", "stream_options"]
        );
        assert!(stub.probed_at.is_empty());

        let caps = ensure(
            dir.path(),
            "p/m",
            &Echo { vision: false },
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert!(
            !caps.probed_at.is_empty(),
            "a stub does not suppress the probe"
        );
        assert_eq!(cached(dir.path(), "p/m").unwrap(), caps);
    }

    #[test]
    fn number_matching_is_whole_run() {
        assert!(mentions_number("The number is 47.", "47"));
        assert!(mentions_number("47", "47"));
        assert!(!mentions_number("It is 147", "47"));
        assert!(!mentions_number("4 7", "47"));
        assert!(!mentions_number("", "47"));
    }

    #[test]
    fn context_window_extraction() {
        let ollama = json!({"model_info": {"llama.context_length": 8192}});
        assert_eq!(context_window_from_info(&ollama, "x"), Some(8192));
        let listing = json!({"data": [{"id": "a", "context_length": 4096}, {"id": "b", "max_model_len": 2048}]});
        assert_eq!(context_window_from_info(&listing, "b"), Some(2048));
        assert_eq!(context_window_from_info(&listing, "zzz"), None);
        let messages_api = json!({"id": "x", "max_input_tokens": 1000000});
        assert_eq!(
            context_window_from_info(&messages_api, "x"),
            Some(1_000_000)
        );
    }
}
