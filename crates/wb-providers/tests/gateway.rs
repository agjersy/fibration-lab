//! Gateway behavior against a local scripted HTTP server (Present PROTOCOL 6.1, 10): auth
//! schemes and extra headers on the wire, the unknown-field downgrade, citation streams,
//! the capability probe, and the audit hook.

mod common;

use base64::Engine;
use common::{key_stores, Recorded, Reply, TestServer};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use wb_providers::audit::MemoryAuditSink;
use wb_providers::model::{REASON_CONTEXT, REASON_TOOLS, REASON_VISION};
use wb_providers::{
    capabilities, client_for, collect, AuditStatus, AuthKind, ChatRequest, ClientOptions,
    ContentPart, Location, Message, ModelCapabilities, ModelEvent, ProviderConfig, ProviderKind,
    Role, StopReason, ToolDef, ToolSupport,
};

const ANTHROPIC_TEXT: &str = include_str!("../src/fixtures/anthropic_text.sse");
const ANTHROPIC_CITATIONS: &str = include_str!("../src/fixtures/anthropic_citations.sse");
const OPENAI_TEXT: &str = include_str!("../src/fixtures/openai_text.sse");

/// The variable the test provider's key is read from (set only in a test's `.env`).
const KEY_VAR: &str = "WB_PROVIDERS_TEST_GATEWAY_KEY";

fn provider(kind: ProviderKind, base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "gateway".into(),
        kind,
        base_url: Some(base_url),
        auth: None,
        key: Some(format!("env:{KEY_VAR}")),
        headers: BTreeMap::new(),
        location: Location::Internal,
    }
}

fn request(text: &str) -> ChatRequest {
    ChatRequest {
        model: "gateway-alias".into(),
        system: Some("You are helpful.".into()),
        messages: vec![Message::user_text(text)],
        tools: vec![ToolDef {
            name: "fs_read".into(),
            description: "read".into(),
            parameters: json!({"type": "object"}),
        }],
        ..Default::default()
    }
}

fn anthropic_text_reply(text: &str) -> Reply {
    let ev = |name: &str, data: Value| format!("event: {name}\ndata: {data}\n\n");
    Reply::sse(
        [
            ev("message_start", json!({"type":"message_start","message":{"usage":{"input_tokens":9,"output_tokens":1}}})),
            ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),
            ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}})),
            ev("content_block_stop", json!({"type":"content_block_stop","index":0})),
            ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}})),
            ev("message_stop", json!({"type":"message_stop"})),
        ]
        .concat(),
    )
}

fn anthropic_tool_reply(name: &str, input: Value) -> Reply {
    let ev = |name: &str, data: Value| format!("event: {name}\ndata: {data}\n\n");
    Reply::sse(
        [
            ev("message_start", json!({"type":"message_start","message":{"usage":{"input_tokens":9,"output_tokens":1}}})),
            ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":name,"input":{}}})),
            ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":input.to_string()}})),
            ev("content_block_stop", json!({"type":"content_block_stop","index":0})),
            ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}})),
            ev("message_stop", json!({"type":"message_stop"})),
        ]
        .concat(),
    )
}

#[tokio::test]
async fn anthropic_bearer_auth_and_extra_headers_are_sent() {
    let server = TestServer::start(|_, _| Reply::sse(ANTHROPIC_TEXT)).await;
    let mut p = provider(ProviderKind::Anthropic, format!("{}/", server.url()));
    p.auth = Some(AuthKind::Bearer);
    p.headers.insert("X-Team".into(), "strategy".into());
    p.headers.insert("X-Cost-Center".into(), "42".into());
    let keys = key_stores(&[(KEY_VAR, "sekrit-token")]);
    let client = client_for(
        &p,
        "gateway-alias",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: None,
        },
    )
    .unwrap();

    let c = collect(client.stream(request("hi"))).await;
    assert_eq!(c.text, "The fit converged.");
    assert_eq!(c.stop, Some(StopReason::End));

    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(
        (r.method.as_str(), r.target.as_str()),
        ("POST", "/v1/messages")
    );
    assert_eq!(r.header("authorization"), Some("Bearer sekrit-token"));
    assert_eq!(r.header("x-api-key"), None, "bearer replaces x-api-key");
    assert_eq!(r.header("x-team"), Some("strategy"));
    assert_eq!(r.header("x-cost-center"), Some("42"));
    assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(r.header("content-type"), Some("application/json"));
    let body = r.json();
    assert_eq!(body["model"], "gateway-alias");
    assert_eq!(body["system"], "You are helpful.");
    assert!(body.get("audit_tag").is_none());
}

#[tokio::test]
async fn anthropic_defaults_to_x_api_key() {
    let server = TestServer::start(|_, _| Reply::sse(ANTHROPIC_TEXT)).await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let keys = key_stores(&[(KEY_VAR, "sekrit-token")]);
    let opts = ClientOptions {
        keys: &keys,
        audit: None,
        home: None,
    };
    let client = client_for(&p, "m", &opts).unwrap();
    collect(client.stream(request("hi"))).await;
    let r = &server.requests()[0];
    assert_eq!(r.header("x-api-key"), Some("sekrit-token"));
    assert_eq!(r.header("authorization"), None);

    // A provider that needs a key but has none configured fails before any request.
    let mut missing = p.clone();
    missing.key = Some("env:WB_PROVIDERS_TEST_UNSET_KEY".into());
    assert!(client_for(&missing, "m", &opts).is_err());
}

#[tokio::test]
async fn openai_compatible_auth_schemes_and_headers() {
    let server = TestServer::start(|_, _| Reply::sse(OPENAI_TEXT)).await;
    let keys = key_stores(&[(KEY_VAR, "sekrit-token")]);
    let opts = ClientOptions {
        keys: &keys,
        audit: None,
        home: None,
    };

    // Default with a key: bearer.
    let mut p = provider(
        ProviderKind::OpenAiCompatible,
        format!("{}/v1", server.url()),
    );
    p.headers.insert("X-Team".into(), "strategy".into());
    let c = collect(client_for(&p, "m", &opts).unwrap().stream(request("hi"))).await;
    assert_eq!(c.text, "Hello, world!");
    assert_eq!((c.input_tokens, c.output_tokens), (10, 3));

    // auth = none: no credential header even though a key is configured.
    p.auth = Some(AuthKind::None);
    collect(client_for(&p, "m", &opts).unwrap().stream(request("hi"))).await;

    // auth = x-api-key on the chat completions wire.
    p.auth = Some(AuthKind::XApiKey);
    collect(client_for(&p, "m", &opts).unwrap().stream(request("hi"))).await;

    let reqs = server.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].target, "/v1/chat/completions");
    assert_eq!(reqs[0].header("authorization"), Some("Bearer sekrit-token"));
    assert_eq!(reqs[0].header("x-team"), Some("strategy"));
    assert_eq!(reqs[0].json()["stream_options"]["include_usage"], true);
    assert_eq!(reqs[1].header("authorization"), None);
    assert_eq!(reqs[1].header("x-api-key"), None);
    assert_eq!(reqs[1].header("x-team"), Some("strategy"));
    assert_eq!(reqs[2].header("x-api-key"), Some("sekrit-token"));
    assert_eq!(reqs[2].header("authorization"), None);
}

fn caps_with(caching: bool, documents: bool) -> ModelCapabilities {
    let mut c = ModelCapabilities {
        context_window: 200_000,
        tools: ToolSupport::Native,
        vision: true,
        documents,
        caching,
        probed_at: "2026-11-15T00:00:00Z".into(),
        ..Default::default()
    };
    c.recompute_degraded();
    c
}

#[tokio::test]
async fn cache_control_rejection_is_retried_once_and_recorded() {
    // The gateway rejects cache_control wherever it appears.
    let server = TestServer::start(|r, _| {
        if r.body_text().contains("cache_control") {
            Reply::json(
                400,
                json!({"type":"error","error":{"type":"invalid_request_error",
                    "message":"system.0.cache_control: Extra inputs are not permitted"}}),
            )
        } else {
            Reply::sse(ANTHROPIC_TEXT)
        }
    })
    .await;
    let home = tempfile::tempdir().unwrap();
    capabilities::store(
        home.path(),
        "gateway/gateway-alias",
        &caps_with(true, false),
    )
    .unwrap();
    let p = provider(ProviderKind::Anthropic, server.url());
    let keys = key_stores(&[(KEY_VAR, "k")]);
    let audit = Arc::new(MemoryAuditSink::default());
    let opts = ClientOptions {
        keys: &keys,
        audit: Some(audit.clone()),
        home: Some(home.path()),
    };
    let client = client_for(&p, "gateway-alias", &opts).unwrap();

    let c = collect(client.stream(request("hi"))).await;
    assert_eq!(c.stop, Some(StopReason::End), "{:?}", c.stop_message);
    assert_eq!(c.text, "The fit converged.");

    let reqs = server.requests();
    assert_eq!(reqs.len(), 2, "one rejected attempt, one retry");
    let first = reqs[0].json();
    assert_eq!(first["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(first["tools"][0]["cache_control"]["type"], "ephemeral");
    let second = reqs[1].json();
    assert_eq!(second["system"], "You are helpful.");
    assert!(second["tools"][0].get("cache_control").is_none());
    assert_eq!(second["messages"], first["messages"]);

    // In memory: the next request does not send the field at all.
    assert_eq!(client.unsupported_fields(), vec!["cache_control"]);
    collect(client.stream(request("again"))).await;
    assert_eq!(server.requests().len(), 3);
    assert!(!server.requests()[2].body_text().contains("cache_control"));

    // Persisted: the cached record lost `caching` and names the field; a new client built
    // from the cache starts downgraded.
    let cached = capabilities::cached(home.path(), "gateway/gateway-alias").unwrap();
    assert!(!cached.caching);
    assert_eq!(cached.unsupported_fields, vec!["cache_control"]);
    assert_eq!(
        cached.context_window, 200_000,
        "the rest of the record is kept"
    );
    let fresh = client_for(&p, "gateway-alias", &opts).unwrap();
    collect(fresh.stream(request("fresh"))).await;
    assert_eq!(server.requests().len(), 4);

    // One audit record per logical request, the retried one included.
    let records = audit.records();
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|r| r.status == AuditStatus::Ok));
}

#[tokio::test]
async fn stream_options_rejection_is_retried_without_it() {
    let server = TestServer::start(|r, _| {
        if r.json().get("stream_options").is_some() {
            Reply::json(
                400,
                json!({"error":{"message":"Unrecognized request argument supplied: stream_options"}}),
            )
        } else {
            Reply::sse(OPENAI_TEXT)
        }
    })
    .await;
    let home = tempfile::tempdir().unwrap();
    let mut p = provider(ProviderKind::OpenAiCompatible, server.url());
    p.key = None;
    let keys = key_stores(&[]);
    let client = client_for(
        &p,
        "m",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: Some(home.path()),
        },
    )
    .unwrap();
    let c = collect(client.stream(request("hi"))).await;
    assert_eq!(c.text, "Hello, world!");
    assert_eq!(server.requests().len(), 2);
    collect(client.stream(request("hi"))).await;
    assert_eq!(server.requests().len(), 3);
    let cached = capabilities::cached(home.path(), "gateway/m").unwrap();
    assert_eq!(cached.unsupported_fields, vec!["stream_options"]);
    assert!(
        cached.probed_at.is_empty(),
        "a downgrade alone is not a probe"
    );
}

#[tokio::test]
async fn other_400s_are_not_retried_and_a_second_rejection_surfaces() {
    // Always 400 naming cache_control, even after it is removed: exactly one retry.
    let server = TestServer::start(|_, _| {
        Reply::json(
            400,
            json!({"error":{"message":"cache_control: Extra inputs are not permitted"}}),
        )
    })
    .await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let keys = key_stores(&[(KEY_VAR, "k")]);
    let mut cfg = wb_providers::EndpointConfig::from_provider(&p, Some("k".into()), "m");
    cfg.capabilities = Some(caps_with(true, false));
    let client = wb_providers::anthropic::AnthropicClient::from_endpoint(cfg).unwrap();
    use wb_providers::ModelClient;
    let c = collect(client.stream(request("hi"))).await;
    assert_eq!(c.stop, Some(StopReason::Error));
    assert!(c.stop_message.unwrap().starts_with("HTTP 400"));
    assert_eq!(server.requests().len(), 2);

    // A 400 that names no optional field we sent is returned as is.
    let server = TestServer::start(|_, _| {
        Reply::json(
            400,
            json!({"error":{"message":"max_tokens: must be positive"}}),
        )
    })
    .await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let client = client_for(
        &p,
        "m",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: None,
        },
    )
    .unwrap();
    let c = collect(client.stream(request("hi"))).await;
    assert_eq!(c.stop, Some(StopReason::Error));
    assert_eq!(server.requests().len(), 1);
}

fn document_request() -> ChatRequest {
    ChatRequest {
        model: "gateway-alias".into(),
        messages: vec![Message {
            role: Role::User,
            content: vec![
                ContentPart::Document {
                    media_type: "application/pdf".into(),
                    data: "JVBERi0xLjQK".into(),
                    title: Some("Annual report".into()),
                },
                ContentPart::Text {
                    text: "How did revenue develop?".into(),
                },
            ],
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn anthropic_stream_with_citation_deltas() {
    let server = TestServer::start(|_, _| Reply::sse(ANTHROPIC_CITATIONS)).await;
    let home = tempfile::tempdir().unwrap();
    capabilities::store(
        home.path(),
        "gateway/gateway-alias",
        &caps_with(false, true),
    )
    .unwrap();
    let p = provider(ProviderKind::Anthropic, server.url());
    let keys = key_stores(&[(KEY_VAR, "k")]);
    let client = client_for(
        &p,
        "gateway-alias",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: Some(home.path()),
        },
    )
    .unwrap();

    use futures::StreamExt;
    let events: Vec<ModelEvent> = client.stream(document_request()).collect().await;
    let citations: Vec<&ModelEvent> = events
        .iter()
        .filter(|e| matches!(e, ModelEvent::Citation { .. }))
        .collect();
    assert_eq!(
        citations,
        vec![
            &ModelEvent::Citation {
                document_index: 0,
                start_page: 3,
                end_page: 4,
                cited_text: "Revenue increased 12% year over year.".into(),
            },
            &ModelEvent::Citation {
                document_index: 1,
                start_page: 7,
                end_page: 9,
                cited_text: "Operating margin: 31%".into(),
            },
        ]
    );
    // Each citation directly follows the text it supports.
    let pos = events
        .iter()
        .position(|e| matches!(e, ModelEvent::Citation { .. }))
        .unwrap();
    assert_eq!(
        events[pos - 1],
        ModelEvent::Text {
            delta: "12% in 2025".into()
        }
    );
    assert_eq!(
        events.last(),
        Some(&ModelEvent::Stop {
            reason: StopReason::End,
            message: None
        })
    );
    let c = collect(client.stream(document_request())).await;
    assert_eq!(c.text, "Revenue grew 12% in 2025 and margins held at 31%.");
    assert_eq!(c.citations.len(), 2);
    assert_eq!(c.citations[0].text_offset, "Revenue grew 12% in 2025".len());
    assert_eq!(c.input_tokens, 5200, "cache-read tokens count as input");

    // The request carried a native document block with citations enabled.
    let body = server.requests()[0].json();
    assert_eq!(
        body["messages"][0]["content"][0],
        json!({
            "type": "document",
            "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERi0xLjQK"},
            "title": "Annual report",
            "citations": {"enabled": true}
        })
    );
}

#[tokio::test]
async fn documents_are_not_sent_as_blocks_without_the_capability() {
    let server = TestServer::start(|_, _| Reply::sse(ANTHROPIC_TEXT)).await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let keys = key_stores(&[(KEY_VAR, "k")]);
    let client = client_for(
        &p,
        "gateway-alias",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: None,
        },
    )
    .unwrap();
    collect(client.stream(document_request())).await;
    let body = server.requests()[0].json();
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert!(!server.requests()[0].body_text().contains("JVBERi0xLjQK"));
}

/// The two-digit number shown in the probe image of a request, if it has one.
fn number_in_image(body: &Value) -> Option<u8> {
    let data = body["messages"][0]["content"]
        .as_array()?
        .iter()
        .find(|b| b["type"] == "image")?["source"]["data"]
        .as_str()?
        .to_string();
    let b64 = &base64::engine::general_purpose::STANDARD;
    (10..=99u8).find(|n| b64.encode(wb_providers::probe_media::vision_probe_png(*n)) == data)
}

/// The code in the probe PDF of a request, if it has a document block.
fn code_in_document(body: &Value) -> Option<String> {
    let data = body["messages"][0]["content"]
        .as_array()?
        .iter()
        .find(|b| b["type"] == "document")?["source"]["data"]
        .as_str()?;
    let pdf = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let text = String::from_utf8_lossy(&pdf).into_owned();
    let start = text.find("(CODE ")? + 6;
    Some(text[start..start + 4].to_string())
}

/// A scripted Anthropic-wire model. `sees` controls whether it reads images; `caching`
/// whether the gateway accepts cache_control.
fn scripted_model(sees: bool, caching: bool) -> impl Fn(&Recorded, usize) -> Reply {
    move |r, _| {
        if !caching && r.body_text().contains("cache_control") {
            return Reply::json(
                400,
                json!({"error":{"message":"tools.0.cache_control: Extra inputs are not permitted"}}),
            );
        }
        let body = r.json();
        if let Some(n) = number_in_image(&body) {
            return if sees {
                anthropic_text_reply(&n.to_string())
            } else {
                anthropic_text_reply("I'm unable to view images, so I can't tell. Maybe 00?")
            };
        }
        if let Some(code) = code_in_document(&body) {
            return anthropic_text_reply(&format!("The code is {code}."));
        }
        if body.get("tools").is_some() {
            let prompt = body["messages"][0]["content"][0]["text"].as_str().unwrap();
            let token = prompt.split('"').nth(1).unwrap().to_string();
            return anthropic_tool_reply("echo_probe", json!({"token": token}));
        }
        anthropic_text_reply("ok")
    }
}

#[tokio::test]
async fn vision_probe_passes_when_the_model_reads_the_number() {
    let server = TestServer::start(scripted_model(true, true)).await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let keys = key_stores(&[(KEY_VAR, "k")]);
    let opts = ClientOptions {
        keys: &keys,
        audit: None,
        home: None,
    };
    let client = client_for(&p, "m", &opts).unwrap();
    assert!(capabilities::probe_vision(client.as_ref()).await);

    // The image on the wire is a 64x64 PNG block.
    let body = server.requests()[0].json();
    let image = &body["messages"][0]["content"][0];
    assert_eq!(image["type"], "image");
    assert_eq!(image["source"]["media_type"], "image/png");
    let png = base64::engine::general_purpose::STANDARD
        .decode(image["source"]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&png[1..4], b"PNG");
    assert_eq!(&png[16..24], &[0, 0, 0, 64, 0, 0, 0, 64]);

    // Same through the chat completions wire (image_url data URL).
    let server = TestServer::start(|r, _| {
        let body = r.json();
        let url = body["messages"][0]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .to_string();
        let data = url.strip_prefix("data:image/png;base64,").unwrap();
        let b64 = &base64::engine::general_purpose::STANDARD;
        let n = (10..=99u8)
            .find(|n| b64.encode(wb_providers::probe_media::vision_probe_png(*n)) == data)
            .unwrap();
        Reply::sse(format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":format!("{n}")},"finish_reason":"stop"}]})
        ))
    })
    .await;
    let mut p = provider(ProviderKind::OpenAiCompatible, server.url());
    p.auth = Some(AuthKind::None);
    let client = client_for(&p, "m", &opts).unwrap();
    assert!(capabilities::probe_vision(client.as_ref()).await);
}

#[tokio::test]
async fn vision_probe_fails_when_the_model_cannot_read_it_or_errors() {
    let keys = key_stores(&[(KEY_VAR, "k")]);
    let opts = ClientOptions {
        keys: &keys,
        audit: None,
        home: None,
    };
    let server = TestServer::start(scripted_model(false, true)).await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let client = client_for(&p, "m", &opts).unwrap();
    assert!(!capabilities::probe_vision(client.as_ref()).await);
    assert_eq!(server.requests().len(), 1);

    // An endpoint that rejects image input outright.
    let server = TestServer::start(|_, _| {
        Reply::json(
            400,
            json!({"error":{"message":"image input is not supported by this model"}}),
        )
    })
    .await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let client = client_for(&p, "m", &opts).unwrap();
    assert!(!capabilities::probe_vision(client.as_ref()).await);
}

#[tokio::test]
async fn full_probe_records_capabilities_and_degraded_tier() {
    let keys = key_stores(&[(KEY_VAR, "k")]);

    // A baseline model behind a gateway that strips prompt caching.
    let server = TestServer::start(scripted_model(true, false)).await;
    let home = tempfile::tempdir().unwrap();
    let p = provider(ProviderKind::Anthropic, server.url());
    let client = client_for(
        &p,
        "m",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: Some(home.path()),
        },
    )
    .unwrap();
    let caps = capabilities::ensure(
        home.path(),
        "gateway/m",
        client.as_ref(),
        None,
        Some(200_000),
        false,
    )
    .await
    .unwrap();
    assert_eq!(caps.tools, ToolSupport::Native);
    assert!(caps.vision && caps.documents);
    assert!(!caps.caching);
    assert_eq!(caps.unsupported_fields, vec!["cache_control"]);
    assert!(!caps.degraded && caps.degraded_reasons.is_empty());
    assert_eq!(
        capabilities::cached(home.path(), "gateway/m").unwrap(),
        caps
    );
    // Tool probe (rejected + retried), vision probe, documents probe.
    assert_eq!(server.requests().len(), 4);
    // After the probe the client sends documents but no caching markers.
    collect(client.stream(document_request())).await;
    let last = server.requests().last().unwrap().json();
    assert_eq!(last["messages"][0]["content"][0]["type"], "document");
    assert!(!last.to_string().contains("cache_control"));

    // A model that supports caching, cannot see, with a small context window.
    let server = TestServer::start(scripted_model(false, true)).await;
    let p = provider(ProviderKind::Anthropic, server.url());
    let client = client_for(
        &p,
        "m",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: None,
        },
    )
    .unwrap();
    let caps = capabilities::probe(client.as_ref(), None, Some(32_000)).await;
    assert!(caps.caching && caps.documents && !caps.vision);
    assert!(caps.unsupported_fields.is_empty());
    assert!(caps.degraded);
    assert_eq!(caps.degraded_reasons, vec![REASON_CONTEXT, REASON_VISION]);
    // The probed record now drives requests: caching markers are sent.
    collect(client.stream(request("hi"))).await;
    assert!(server
        .requests()
        .last()
        .unwrap()
        .body_text()
        .contains("cache_control"));

    // A chat-completions model that answers in prose: text protocol, no documents/caching.
    let server = TestServer::start(|_, _| Reply::sse(OPENAI_TEXT)).await;
    let mut p = provider(ProviderKind::OpenAiCompatible, server.url());
    p.auth = Some(AuthKind::None);
    let client = client_for(
        &p,
        "m",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: None,
        },
    )
    .unwrap();
    let caps = capabilities::probe(client.as_ref(), None, None).await;
    assert_eq!(caps.tools, ToolSupport::TextProtocol);
    assert!(!caps.vision && !caps.documents && !caps.caching);
    assert_eq!(
        caps.degraded_reasons,
        vec![REASON_TOOLS, REASON_CONTEXT, REASON_VISION]
    );
    assert_eq!(
        server.requests().len(),
        2,
        "no documents probe on this wire"
    );
}

#[tokio::test]
async fn audit_records_carry_metadata_and_never_content_keys_or_headers() {
    const SENTINEL: &str = "SENTINEL-prompt-text-7f3a";
    const KEY: &str = "sekrit-key-91c2";
    const HEADER_VALUE: &str = "header-value-55d0";
    let server = TestServer::start(move |r, seq| match seq {
        0 => Reply::sse(ANTHROPIC_TEXT),
        // An error body that echoes the prompt back.
        1 => Reply::json(
            429,
            json!({"error":{"type":"rate_limit_error","message":format!("too many requests for: {}", r.body_text())}}),
        ),
        2 => Reply::sse(format!(
            "event: error\ndata: {}\n\n",
            json!({"type":"error","error":{"type":"overloaded_error","message":format!("Overloaded while handling {SENTINEL}")}})
        )),
        _ => Reply::sse(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":0}}}\n\n\
             event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"usage\":{\"output_tokens\":2}}\n\n\
             event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
    })
    .await;
    let mut p = provider(ProviderKind::Anthropic, server.url());
    p.headers.insert("X-Team".into(), HEADER_VALUE.into());
    let keys = key_stores(&[(KEY_VAR, KEY)]);
    let audit = Arc::new(MemoryAuditSink::default());
    let client = client_for(
        &p,
        "gateway-alias",
        &ClientOptions {
            keys: &keys,
            audit: Some(audit.clone()),
            home: None,
        },
    )
    .unwrap();

    for i in 0..4 {
        let mut req = request(SENTINEL);
        req.system = Some(format!("system {SENTINEL}"));
        req.audit_tag = Some(format!("turn-{i}"));
        let c = collect(client.stream(req)).await;
        // The record is written before the stream's final event is delivered.
        assert_eq!(audit.records().len(), i + 1);
        if i == 1 {
            assert!(
                c.stop_message.unwrap().contains(SENTINEL),
                "the caller still sees the body"
            );
        }
    }
    let records = audit.records();
    let r = &records[0];
    assert_eq!(r.provider, "gateway");
    assert_eq!(r.location, Location::Internal);
    assert_eq!(r.base_url_host, "127.0.0.1");
    assert_eq!(r.model, "gateway-alias");
    assert_eq!((r.input_tokens, r.output_tokens), (40, 8));
    assert_eq!((r.status, r.error.as_deref()), (AuditStatus::Ok, None));
    assert_eq!(r.tag.as_deref(), Some("turn-0"));
    assert!(chrono::DateTime::parse_from_rfc3339(&r.ts).is_ok());
    assert_eq!(
        (records[1].status, records[1].error.as_deref()),
        (AuditStatus::Error, Some("http_429"))
    );
    assert_eq!(
        (records[2].status, records[2].error.as_deref()),
        (AuditStatus::Error, Some("overloaded_error"))
    );
    assert_eq!(
        (records[3].status, records[3].error.as_deref()),
        (AuditStatus::Refused, None)
    );
    assert_eq!((records[3].input_tokens, records[3].output_tokens), (12, 2));

    let dump = serde_json::to_string(&records).unwrap();
    for secret in [SENTINEL, KEY, HEADER_VALUE, "X-Team", "You are helpful"] {
        assert!(
            !dump.contains(secret),
            "audit log leaked `{secret}`: {dump}"
        );
    }
    let keys_seen: std::collections::BTreeSet<String> = serde_json::to_value(&records)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r.as_object().unwrap().keys().cloned().collect::<Vec<_>>())
        .collect();
    let allowed = [
        "ts",
        "provider",
        "location",
        "base_url_host",
        "model",
        "input_tokens",
        "output_tokens",
        "status",
        "error",
        "tag",
    ];
    assert!(
        keys_seen.iter().all(|k| allowed.contains(&k.as_str())),
        "{keys_seen:?}"
    );
}

#[tokio::test]
async fn transport_failure_is_a_stop_error_and_an_audit_error() {
    // Nothing listens on this port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut p = provider(ProviderKind::OpenAiCompatible, format!("http://{addr}"));
    p.auth = Some(AuthKind::None);
    let keys = key_stores(&[]);
    let audit = Arc::new(MemoryAuditSink::default());
    let client = client_for(
        &p,
        "m",
        &ClientOptions {
            keys: &keys,
            audit: Some(audit.clone()),
            home: None,
        },
    )
    .unwrap();
    let c = collect(client.stream(request("hi"))).await;
    assert_eq!(c.stop, Some(StopReason::Error));
    assert!(c.stop_message.unwrap().starts_with("request failed"));
    let records = audit.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].error.as_deref(), Some("transport"));
    assert_eq!(records[0].location, Location::Internal);
}

#[tokio::test]
async fn an_endpoint_that_is_not_allowed_at_the_gate_is_never_contacted() {
    // A listener that the gate has not been told about: built directly from an endpoint
    // config, the client must be refused before any connection is made.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut p = provider(ProviderKind::OpenAiCompatible, format!("http://{addr}"));
    p.auth = Some(AuthKind::None);
    let audit = Arc::new(MemoryAuditSink::default());
    let mut cfg = wb_providers::EndpointConfig::from_provider(&p, None, "m");
    cfg.audit = Some(audit.clone());
    let client = wb_providers::openai_compat::OpenAiCompatClient::from_endpoint(cfg).unwrap();
    use wb_providers::ModelClient;
    let c = collect(client.stream(request("hi"))).await;
    assert_eq!(c.stop, Some(StopReason::Error));
    let message = c.stop_message.unwrap();
    assert!(
        message.contains("was not sent") && message.contains("not allowed"),
        "{message}"
    );
    assert_eq!(audit.records()[0].error.as_deref(), Some("egress_denied"));
    assert!(client.list_models().await.is_err());
    let accepted = tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept());
    assert!(accepted.await.is_err(), "nothing may connect");

    // `client_for` allows the configured base URL, and only that host and port.
    let keys = key_stores(&[]);
    client_for(
        &p,
        "m",
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: None,
        },
    )
    .unwrap();
    assert!(wb_net::gate().is_allowed(
        wb_net::EgressClass::ModelEndpoint,
        &format!("http://{addr}/v1")
    ));
    assert!(!wb_net::gate().is_allowed(wb_net::EgressClass::ModelEndpoint, "http://127.0.0.1:1/v1"));
}
