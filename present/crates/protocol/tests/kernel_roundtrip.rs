//! Round trips of kernel method parameters and results, notifications, and error data
//! (PROTOCOL 1).

mod common;

use common::{fixture_json, round_trip_json, round_trip_jsonl, round_trip_value};
use fibration_deck_protocol::kernel::*;
use fibration_deck_protocol::template::TemplateSidecar;
use serde_json::{json, Value};

/// Round-trips the fixture's `params` and `result` for one method.
fn check<M: Method>(all: &Value, seen: &mut Vec<&'static str>) {
    let entry = all
        .get(M::NAME)
        .unwrap_or_else(|| panic!("no fixture for {}", M::NAME));
    let params: M::Params = round_trip_value(&format!("{} params", M::NAME), &entry["params"]);
    let result = match entry["result"].as_str().and_then(|s| s.strip_prefix('@')) {
        Some(file) => fixture_json(file),
        None => entry["result"].clone(),
    };
    round_trip_value::<M::Result>(&format!("{} result", M::NAME), &result);
    // The envelope carries the same params under the method's wire name.
    let request = M::request(1, &params).unwrap();
    assert_eq!(request.method, M::NAME);
    assert_eq!(request.jsonrpc, "2.0");
    assert_eq!(request.params.as_ref(), Some(&entry["params"]));
    seen.push(M::NAME);
}

#[test]
fn every_method_round_trips() {
    let all = fixture_json("kernel_methods.json");
    let mut seen = Vec::new();
    check::<KernelHello>(&all, &mut seen);
    check::<KernelShutdown>(&all, &mut seen);
    check::<TemplateInspect>(&all, &mut seen);
    check::<TemplateThumbnails>(&all, &mut seen);
    check::<TemplateSidecarMethod>(&all, &mut seen);
    check::<DeckValidate>(&all, &mut seen);
    check::<DeckRender>(&all, &mut seen);
    check::<DeckEject>(&all, &mut seen);
    check::<SlideGenerate>(&all, &mut seen);
    check::<DeckBuild>(&all, &mut seen);
    check::<RenderFidelity>(&all, &mut seen);
    check::<CheckRun>(&all, &mut seen);
    check::<TextMeasure>(&all, &mut seen);
    check::<RefsIngest>(&all, &mut seen);
    check::<RefsRead>(&all, &mut seen);
    check::<RefsTable>(&all, &mut seen);
    check::<RefsPageImage>(&all, &mut seen);
    check::<EmbedTexts>(&all, &mut seen);
    check::<ChartCompute>(&all, &mut seen);
    check::<ExportOutlineMd>(&all, &mut seen);
    check::<ExportNotesDocx>(&all, &mut seen);

    // Every method of PROTOCOL 1.3 is declared, checked, and has a fixture; nothing else.
    assert_eq!(seen, METHOD_NAMES);
    let mut fixture_names: Vec<&str> = all
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut declared = METHOD_NAMES.to_vec();
    fixture_names.sort_unstable();
    declared.sort_unstable();
    assert_eq!(fixture_names, declared);
    assert_eq!(declared.len(), 21);
}

#[test]
fn optional_params_may_be_omitted() {
    let p: DeckValidateParams = round_trip_value("deck.validate {}", &json!({}));
    assert_eq!(p.deck, None);
    round_trip_value::<TemplateThumbnailsParams>(
        "thumbnails",
        &json!({ "path": "template/t.potx" }),
    );
    round_trip_value::<DeckRenderParams>(
        "render",
        &json!({ "out": "o.pptx", "snapshot_id": "h-0a1b2c3d4e" }),
    );
    round_trip_value::<RenderFidelityParams>(
        "fidelity",
        &json!({ "pptx": "o.pptx", "format": "pdf", "out_dir": "x" }),
    );
    round_trip_value::<TextMeasureParams>(
        "measure",
        &json!({ "font_family": "Noto Sans", "size_pt": 12.0, "paragraphs": [], "box": { "cx": 1, "cy": 1 } }),
    );
    round_trip_value::<TextMeasureResult>(
        "measure result",
        &json!({ "fits": true, "lines": 1, "height": 200000, "font_found": false }),
    );
    round_trip_value::<DeckEjectResult>(
        "eject",
        &json!({ "script": "deck/build.py", "verified": true }),
    );
    // A snapshot id that does not match its pattern is rejected at the boundary.
    let bad = json!({ "out": "o.pptx", "snapshot_id": "HEAD" });
    assert!(serde_json::from_value::<DeckRenderParams>(bad).is_err());
}

#[test]
fn template_sidecar_result_may_be_an_object() {
    let v = json!({ "template": { "name": "Corp 2026" }, "rules": { "closing": "Closing" } });
    let r: Option<TemplateSidecar> = round_trip_value("template.sidecar result", &v);
    assert_eq!(r.unwrap().rules.closing.as_deref(), Some("Closing"));
}

#[test]
fn hello_enums_cover_every_documented_value() {
    for os in ["windows", "macos", "linux"] {
        round_trip_value::<Os>("os", &json!(os));
    }
    for kind in ["sandbox-exec", "appcontainer", "bwrap", "limits"] {
        round_trip_value::<SandboxKind>("sandbox", &json!(kind));
    }
    for kind in ["powerpoint", "libreoffice"] {
        round_trip_value::<EngineKind>("engine", &json!(kind));
    }
    for kind in ["auto", "powerpoint", "libreoffice"] {
        round_trip_value::<EngineChoice>("engine choice", &json!(kind));
    }
    assert!(serde_json::from_value::<EngineChoice>(json!("none")).is_err());
    for kind in ["bundled", "endpoint", "none"] {
        round_trip_value::<EmbeddingKind>("embedding", &json!(kind));
    }
    for kind in [
        "pdf",
        "docx",
        "pptx",
        "xlsx",
        "csv",
        "image",
        "markdown",
        "text",
        "eml",
        "msg",
        "html",
        "analyst-figure",
        "analyst-table",
    ] {
        round_trip_value::<RefKind>("ref kind", &json!(kind));
    }
    for sev in ["error", "warn", "info"] {
        round_trip_value::<Severity>("severity", &json!(sev));
    }
    assert!(Severity::Error > Severity::Warn && Severity::Warn > Severity::Info);
}

#[test]
fn notifications_round_trip_typed_and_raw() {
    let raw: Vec<RawNotification> = round_trip_jsonl("kernel_notifications.jsonl");
    assert_eq!(raw.len(), 6);
    let typed: Vec<Notification> = raw
        .iter()
        .map(|r| Notification::from_raw(r).unwrap())
        .collect();
    assert_eq!(
        typed[0],
        Notification::TaskProgress(TaskProgress {
            request_id: RequestId::Number(7),
            fraction: Some(0.5),
            message: "page 12 of 24".into()
        })
    );
    assert!(matches!(
        &typed[1],
        Notification::TaskProgress(TaskProgress { request_id: RequestId::String(s), fraction: None, .. }) if s == "req-9"
    ));
    let levels: Vec<LogLevel> = typed[2..]
        .iter()
        .map(|n| match n {
            Notification::Log(l) => l.level,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        levels,
        [
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error
        ]
    );
    // The typed form serializes to the envelope's method and params.
    for (r, t) in raw.iter().zip(&typed) {
        let v = serde_json::to_value(t).unwrap();
        assert_eq!(v["method"], json!(r.method));
        assert_eq!(Some(&v["params"]), r.params.as_ref());
        assert_eq!(&serde_json::from_value::<Notification>(v).unwrap(), t);
    }
    // Each line is an `Incoming::Notification`.
    for line in common::fixture("kernel_notifications.jsonl").lines() {
        assert!(matches!(
            serde_json::from_str::<Incoming>(line).unwrap(),
            Incoming::Notification(_)
        ));
    }
    let unknown = RawNotification {
        jsonrpc: "2.0".into(),
        method: "run.stdout".into(),
        params: None,
    };
    assert_eq!(Notification::from_raw(&unknown), None);
}

#[test]
fn error_data_shapes() {
    let errors: Vec<RpcError> = round_trip_json("kernel_errors.json");
    for e in &errors {
        assert!(e.detail().is_some(), "error.data always has detail");
        let base: ErrorData = e.data_as().unwrap();
        assert_eq!(Some(base.detail.as_str()), e.detail());
    }
    assert_eq!(errors[0].code, codes::NOT_FOUND);
    assert_eq!(errors[1].code, codes::SCHEMA_INVALID);
    let schema: SchemaErrorData = round_trip_value("2002", errors[1].data.as_ref().unwrap());
    assert_eq!(schema.errors.len(), 2);
    assert_eq!(schema.errors[0].pointer, "/placeholders/body/type");
    assert_eq!(errors[2].code, codes::RENDER_FAILED);
    let render: RenderErrorData = round_trip_value("2004", errors[2].data.as_ref().unwrap());
    assert_eq!(render.element_id.as_deref(), Some("e-im01"));
    let render: RenderErrorData = round_trip_value("2004 bare", errors[3].data.as_ref().unwrap());
    assert_eq!(render.element_id, None);
    assert_eq!(errors[4].code, codes::BUILD_FAILED);
    let build: BuildErrorData = round_trip_value("2005", errors[4].data.as_ref().unwrap());
    assert_eq!(build.exit_code, 1);
    assert_eq!(errors[5].code, codes::TRANSFORM_FAILED);
    let transform: TransformErrorData = round_trip_value("2010", errors[5].data.as_ref().unwrap());
    assert_eq!(transform.step_index, 1);
}

#[test]
fn error_codes_match_protocol_1_2() {
    let table = [
        (codes::NOT_FOUND, 2000),
        (codes::PATH_OUTSIDE_ROOT, 2001),
        (codes::SCHEMA_INVALID, 2002),
        (codes::TEMPLATE_UNSUPPORTED, 2003),
        (codes::RENDER_FAILED, 2004),
        (codes::BUILD_FAILED, 2005),
        (codes::TIMEOUT, 2006),
        (codes::SANDBOX_UNAVAILABLE, 2007),
        (codes::ENGINE_UNAVAILABLE, 2008),
        (codes::REF_FORMAT_UNSUPPORTED, 2009),
        (codes::TRANSFORM_FAILED, 2010),
        (codes::PARSE_ERROR, -32700),
        (codes::INVALID_REQUEST, -32600),
        (codes::METHOD_NOT_FOUND, -32601),
        (codes::INVALID_PARAMS, -32602),
        (codes::INTERNAL_ERROR, -32603),
    ];
    for (constant, documented) in table {
        assert_eq!(constant, documented);
    }
    assert_eq!(fibration_deck_protocol::PROTOCOL_VERSION, 1);
}

#[test]
fn responses_round_trip() {
    let ok = json!({ "jsonrpc": "2.0", "id": 3, "result": { "out": "exports/outline.md" } });
    let r: Response = round_trip_value("response", &ok);
    assert_eq!(r.id, RequestId::Number(3));
    let err = json!({ "jsonrpc": "2.0", "id": "a", "error": { "code": 2006, "message": "timeout", "data": { "detail": "60 s" } } });
    let r: Response = round_trip_value("error response", &err);
    assert_eq!(r.error.unwrap().code, codes::TIMEOUT);
    // `kernel.shutdown` answers `result: null`; the envelope reads that as no result value.
    let null: Incoming =
        serde_json::from_value(json!({ "jsonrpc": "2.0", "id": 9, "result": null })).unwrap();
    assert!(matches!(
        null,
        Incoming::Response(Response {
            result: None,
            error: None,
            ..
        })
    ));
}
