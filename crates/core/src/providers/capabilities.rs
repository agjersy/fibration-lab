//! Capability probe and `~/.analyst/capabilities.json` cache (PROTOCOL 4).

use super::{collect, ModelClient};
use analyst_protocol::config::{ModelConfig, ToolSupport};
use analyst_protocol::model::StopReason;
use analyst_protocol::model::{ChatRequest, Message, ModelCapabilities, ToolDef};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONTEXT_WINDOW: u32 = 32768;

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
    }
    caps
}

/// Run the tool-calling probe against a client. `context_window` comes from the override or
/// the model-info value when given, else the default. A probe that could not be completed
/// (request or provider error) is an `Err`: it says nothing about the model and must not be
/// recorded.
pub async fn probe(
    client: &dyn ModelClient,
    overrides: Option<&ModelConfig>,
    info_context_window: Option<u32>,
) -> Result<ModelCapabilities> {
    let token = format!("probe-{}", crate::util::short_rand());
    let req = ChatRequest {
        model: client.model_id().to_string(),
        system: Some("You are a tool-calling test harness. Follow the instruction exactly.".into()),
        messages: vec![Message::user_text(format!(
            "Call the echo_probe tool with token \"{token}\". Do not reply with text."
        ))],
        tools: vec![probe_tool()],
        max_tokens: Some(1024),
    };
    let c = collect(client.stream(req)).await;
    if c.stop == Some(StopReason::Error) || c.stop.is_none() {
        return Err(anyhow!(
            "capability probe failed: {}",
            c.stop_message.unwrap_or_else(|| "no response".into())
        ));
    }
    let native = c.tool_calls.iter().any(|(_, name, args)| {
        name == "echo_probe" && args.get("token").and_then(Value::as_str) == Some(token.as_str())
    });
    Ok(apply_overrides(
        ModelCapabilities {
            context_window: info_context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
            tools: if native {
                ToolSupport::Native
            } else {
                ToolSupport::TextProtocol
            },
            vision: false,
            probed_at: crate::util::now_rfc3339(),
            max_output_tokens: None,
        },
        overrides,
    ))
}

/// Pull a context window out of a model-info JSON document from either provider family
/// (Anthropic `max_input_tokens`; `context_length`, `context_window`, `max_model_len`, Ollama
/// `model_info.*.context_length`).
pub fn context_window_from_info(info: &Value, model_id: &str) -> Option<u32> {
    fn scan(v: &Value) -> Option<u32> {
        match v {
            Value::Object(m) => {
                for (k, val) in m {
                    if (k == "context_length"
                        || k == "max_input_tokens"
                        || k == "context_window"
                        || k == "max_model_len"
                        || k == "max_context_length"
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

/// Capabilities for a model, from cache or by probing. Config overrides are applied on top in
/// both cases. Only a completed probe is cached; when the probe fails the error is returned
/// (unless the config already says how the model takes tools, in which case no probe is
/// needed at all) and the next call probes again.
pub async fn ensure(
    home: &Path,
    key: &str,
    client: &dyn ModelClient,
    overrides: Option<&ModelConfig>,
    info_context_window: Option<u32>,
    force: bool,
) -> Result<ModelCapabilities> {
    if !force {
        if let Some(c) = cached(home, key) {
            return Ok(apply_overrides(c, overrides));
        }
    }
    if let Some(o) = overrides.filter(|o| o.tools.is_some()) {
        // Nothing to learn from a probe; do not spend a request or depend on the network.
        return Ok(apply_overrides(
            ModelCapabilities {
                context_window: info_context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
                probed_at: String::new(),
                ..Default::default()
            },
            Some(o),
        ));
    }
    let caps = probe(client, None, info_context_window).await?;
    store(home, key, &caps)?;
    Ok(apply_overrides(caps, overrides))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ScriptedModelClient;
    use analyst_protocol::model::ModelEvent;

    #[tokio::test]
    async fn probe_detects_native_and_text_protocol() {
        let native = ScriptedModelClient::new(vec![]);
        // The scripted client cannot know the token in advance, so emulate by inspecting the
        // request after the fact: run once with a wrong token (text-protocol) and once with a
        // client that echoes whatever it is asked.
        let caps = probe(&native, None, None).await.unwrap();
        assert_eq!(caps.tools, ToolSupport::TextProtocol);
        assert_eq!(caps.context_window, DEFAULT_CONTEXT_WINDOW);

        struct Echo;
        impl ModelClient for Echo {
            fn stream(&self, req: ChatRequest) -> futures::stream::BoxStream<'static, ModelEvent> {
                let text = req.messages[0].text();
                let token = text.split('"').nth(1).unwrap_or("").to_string();
                Box::pin(futures::stream::iter(vec![
                    ModelEvent::ToolCall {
                        id: "1".into(),
                        name: "echo_probe".into(),
                        arguments: json!({"token": token}),
                    },
                    ModelEvent::Stop {
                        reason: StopReason::ToolCalls,
                        message: None,
                    },
                ]))
            }
            fn model_id(&self) -> &str {
                "echo"
            }
        }
        let caps = probe(&Echo, None, Some(65536)).await.unwrap();
        assert_eq!(caps.tools, ToolSupport::Native);
        assert_eq!(caps.context_window, 65536);

        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "p/m", &caps).unwrap();
        let back = cached(dir.path(), "p/m").unwrap();
        assert_eq!(back, caps);
        let ensured = ensure(dir.path(), "p/m", &native, None, None, false)
            .await
            .unwrap();
        assert_eq!(ensured.tools, ToolSupport::Native, "cache wins");
    }

    #[test]
    fn context_window_extraction() {
        let ollama = json!({"model_info": {"llama.context_length": 8192}});
        assert_eq!(context_window_from_info(&ollama, "x"), Some(8192));
        let listing = json!({"data": [{"id": "a", "context_length": 4096}, {"id": "b", "max_model_len": 2048}]});
        assert_eq!(context_window_from_info(&listing, "b"), Some(2048));
        assert_eq!(context_window_from_info(&listing, "zzz"), None);
    }

    struct Failing;
    impl ModelClient for Failing {
        fn stream(&self, req: ChatRequest) -> futures::stream::BoxStream<'static, ModelEvent> {
            assert!(serde_json::to_value(&req)
                .unwrap()
                .get("temperature")
                .is_none());
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
            &ScriptedModelClient::new(vec![]),
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(ok.tools, ToolSupport::TextProtocol);
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
        assert_eq!(with.tools, ToolSupport::Native);
        assert_eq!(with.context_window, 200_000);
        assert!(with.vision);
        assert_eq!(with.max_output_tokens, Some(8000));
        assert_eq!(
            cached(dir.path(), "p/m").unwrap().tools,
            ToolSupport::TextProtocol
        );
        // With `tools` configured, an unreachable provider does not block anything.
        let fresh = tempfile::tempdir().unwrap();
        let caps = ensure(fresh.path(), "p/m", &Failing, Some(&o), None, false)
            .await
            .unwrap();
        assert_eq!(caps.tools, ToolSupport::Native);
    }

    #[test]
    fn anthropic_model_info_window() {
        let info = json!({"id": "claude-x", "max_input_tokens": 200000, "max_tokens": 64000});
        assert_eq!(context_window_from_info(&info, "claude-x"), Some(200000));
    }
}
