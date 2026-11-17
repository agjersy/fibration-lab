//! Round trips of the template description and sidecar (PROTOCOL 3, 6.4), the
//! configuration and project files (4), transcript events (6.2), the audit record (10),
//! and the evaluation task file (12).

mod common;

use common::{fixture, round_trip_json, round_trip_jsonl, round_trip_toml, round_trip_value};
use present_protocol::audit::*;
use present_protocol::config::*;
use present_protocol::deck::{DeckMode, ThemeColor};
use present_protocol::evals::*;
use present_protocol::kernel::{RefKind, Severity};
use present_protocol::template::*;
use present_protocol::transcript::{DecidedBy, Decision, Event, ProposalKind};
use serde_json::json;

// ---- template ----

#[test]
fn template_description_round_trips() {
    let t: TemplateDescription = round_trip_json("template_description.json");
    assert_eq!(t.theme.colors.len(), ThemeColor::ALL.len());
    assert_eq!(t.theme.colors[&ThemeColor::FolHlink], "954F72");
    assert_eq!(t.theme.fonts.cs, None);
    assert_eq!(t.layouts().count(), 2);
    let layout = t.layout("Title and Content").unwrap();
    assert!(layout.usable);
    let body = &layout.placeholders[1];
    assert_eq!(body.key, "body");
    assert_eq!(body.r#type, PlaceholderType::Body);
    assert_eq!(body.autofit, Autofit::None);
    assert_eq!(body.anchor, "t");
    assert_eq!(body.levels[1].mar_l, 685800);
    assert_eq!(
        t.default_table_style.as_deref(),
        Some("{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}")
    );
    assert_eq!(body.levels[0].color, LevelColor::Theme(ThemeColor::Dk1));
    assert_eq!(body.levels[1].color, LevelColor::Hex("7F7F7F".into()));
    assert_eq!(body.accepts.len(), 5);
    assert_eq!(layout.placeholders[2].r#type, PlaceholderType::SldNum);
    assert!(!t.layout("Blank").unwrap().usable);
    assert!(t.layout("Missing").is_none());
}

#[test]
fn placeholder_types_use_the_ooxml_names() {
    for name in [
        "title", "ctrTitle", "subTitle", "body", "obj", "pic", "chart", "tbl", "dgm", "media",
        "clipArt", "dt", "ftr", "sldNum",
    ] {
        round_trip_value::<PlaceholderType>("placeholder type", &json!(name));
    }
    for name in ["none", "shrink", "resize"] {
        round_trip_value::<Autofit>("autofit", &json!(name));
    }
    for name in ["text", "image", "chart", "table", "diagram"] {
        round_trip_value::<ContentKind>("accepts", &json!(name));
    }
    assert!(serde_json::from_value::<ContentKind>(json!("empty")).is_err());
}

#[test]
fn template_sidecar_round_trips() {
    let s: TemplateSidecar = round_trip_toml("template.toml");
    assert_eq!(s.template.name.as_deref(), Some("Corp 2026"));
    assert_eq!(s.layouts["Title and Content"].max_bullets, Some(5));
    assert_eq!(
        s.layouts["Layout 7"].description.as_deref(),
        Some("Big number with caption")
    );
    assert_eq!(s.layouts["Blank"].avoid, Some(true));
    assert_eq!(s.rules.mandatory, ["Title Slide", "Disclaimer"]);
    assert_eq!(s.rules.closing.as_deref(), Some("Closing"));
    assert_eq!(s.rules.allow_free_elements, Some(false));
    assert_eq!(s.rules.icons_dir.as_deref(), Some("icons"));
    // Every table is optional.
    let empty: TemplateSidecar = toml::from_str("").unwrap();
    assert_eq!(empty, TemplateSidecar::default());
}

// ---- configuration ----

#[test]
fn global_config_round_trips() {
    let c: GlobalConfig = round_trip_toml("config.toml");
    assert_eq!(c.defaults.model.as_deref(), Some("gateway/claude-opus-5-5"));
    assert_eq!(c.network.proxy, "system");
    assert_eq!(c.runtime_python, None);
    let gateway = c.provider("gateway").unwrap();
    assert_eq!(gateway.kind, ProviderKind::Anthropic);
    assert_eq!(gateway.effective_auth(), AuthKind::Bearer);
    assert_eq!(
        gateway.key_source(),
        Some(KeySource::Keychain("gateway".into()))
    );
    assert_eq!(gateway.headers["X-Team"], "strategy");
    assert_eq!(gateway.effective_location(), Location::Internal);
    let gpu = c.provider("gpu").unwrap();
    assert_eq!(gpu.kind, ProviderKind::OpenAiCompatible);
    assert_eq!(gpu.auth, Some(AuthKind::None));
    assert_eq!(gpu.key_source(), None);
    assert_eq!(c.models[0].context_window, Some(1_000_000));
    assert_eq!(c.models[0].vision, Some(true));
    assert_eq!(c.models[0].max_output_tokens, Some(64_000));
    assert_eq!(c.embedding.kind, EmbeddingSource::Bundled);
    assert_eq!(c.render.engine, RenderEngine::Auto);
    assert_eq!(c.render.libreoffice_path, "");
    assert_eq!(c.ui.theme, UiTheme::System);
}

#[test]
fn global_config_defaults_and_variants() {
    let empty: GlobalConfig = toml::from_str("").unwrap();
    assert_eq!(empty, GlobalConfig::default());
    assert_eq!(empty.network.ca_bundle, "system");
    // Defaults survive a round trip through text.
    let again: GlobalConfig = toml::from_str(&toml::to_string(&empty).unwrap()).unwrap();
    assert_eq!(again, empty);

    let text = r#"
runtime_python = "/opt/present/python/bin/python3"

[network]
proxy = "http://proxy.corp:8080"
ca_bundle = "/etc/ssl/corp.pem"

[[providers]]
name = "direct"
kind = "anthropic"
key = "env:ANTHROPIC_API_KEY"

[embedding]
kind = "endpoint"
provider = "gpu"
model = "bge-m3"

[render]
engine = "none"

[ui]
theme = "dark"
"#;
    let c: GlobalConfig = toml::from_str(text).unwrap();
    assert_eq!(
        c.runtime_python.as_deref(),
        Some("/opt/present/python/bin/python3")
    );
    let direct = c.provider("direct").unwrap();
    assert_eq!(direct.effective_auth(), AuthKind::XApiKey);
    assert_eq!(direct.effective_location(), Location::External);
    assert_eq!(
        direct.key_source(),
        Some(KeySource::Env("ANTHROPIC_API_KEY".into()))
    );
    assert_eq!(c.embedding.kind, EmbeddingSource::Endpoint);
    assert_eq!(c.embedding.model.as_deref(), Some("bge-m3"));
    assert_eq!(c.render.engine, RenderEngine::None);
    assert_eq!(c.ui.theme, UiTheme::Dark);
    let again: GlobalConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
    assert_eq!(again, c);

    for (auth, expected) in [
        ("x-api-key", AuthKind::XApiKey),
        ("bearer", AuthKind::Bearer),
        ("none", AuthKind::None),
    ] {
        assert_eq!(
            serde_json::from_value::<AuthKind>(json!(auth)).unwrap(),
            expected
        );
    }
    assert_eq!(KeySource::parse("keychain:"), None);
    assert_eq!(KeySource::parse("sk-secret"), None);
    assert_eq!(KeySource::parse("file:/tmp/key"), None);
    assert!(toml::from_str::<GlobalConfig>("[render]\nengine = \"keynote\"\n").is_err());
}

#[test]
fn project_config_round_trips() {
    let p: ProjectConfig = round_trip_toml("project.toml");
    assert_eq!(p.project.name, "Q3 review");
    assert_eq!(p.project.created, "2026-11-15T10:00:00Z");
    // The documented example is also the set of defaults.
    assert_eq!(p.policy, PolicyConfig::default());
    assert_eq!(p.contract.max_bullets, 6);
    assert_eq!(p.contract.max_words, 70);
    assert_eq!(p.contract.min_font_pt, 12);
    assert!(!p.contract.allow_free_elements);
    assert_eq!(p.contract.severities.len(), 2);
    assert_eq!(p.contract.severities["density.bullets"], Severity::Warn);
    assert_eq!(p.contract.severities["fit.overflow"], Severity::Error);
    assert_eq!(p.export_presets.len(), 1);
    assert_eq!(p.export_presets[0].dpi, Some(150));
    assert_eq!(p.export_presets[0].file, "{project}-{date}-preread.pdf");

    let empty: ProjectConfig = toml::from_str("").unwrap();
    assert_eq!(empty, ProjectConfig::default());
    assert_eq!(empty.policy.build_timeout_s, 60);
    assert_eq!(empty.contract, p.contract_without_overrides());
}

trait WithoutOverrides {
    fn contract_without_overrides(&self) -> ContractConfig;
}

impl WithoutOverrides for ProjectConfig {
    fn contract_without_overrides(&self) -> ContractConfig {
        ContractConfig {
            severities: Default::default(),
            ..self.contract.clone()
        }
    }
}

#[test]
fn policy_values() {
    let text = r#"
[policy]
edits = "auto-new-slides"
code = "deny"
skills_scripts = "auto"
code_mode = "deny"
mcp = "deny"
export = "ask"
max_steps = 10
build_timeout_s = 120

[contract]
"src.coverage" = "info"
"#;
    let p: ProjectConfig = toml::from_str(text).unwrap();
    assert_eq!(p.policy.edits, EditPolicy::AutoNewSlides);
    assert_eq!(p.policy.code, Gate::Deny);
    assert_eq!(p.policy.skills_scripts, Gate::Auto);
    assert_eq!(p.policy.code_mode, Permission::Deny);
    assert_eq!(p.policy.export, Gate::Ask);
    assert_eq!(p.contract.severities["src.coverage"], Severity::Info);
    assert_eq!(p.contract.max_bullets, 6);
    let again: ProjectConfig = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
    assert_eq!(again, p);
    for edits in ["review", "auto-new-slides", "auto"] {
        round_trip_value::<EditPolicy>("edits", &json!(edits));
    }
    assert!(toml::from_str::<ProjectConfig>("[policy]\nedits = \"yolo\"\n").is_err());
    assert!(toml::from_str::<ProjectConfig>("[contract]\n\"fit.overflow\" = \"fatal\"\n").is_err());
}

#[test]
fn reference_manifest_round_trips() {
    let m: RefsManifest = round_trip_toml("manifest.toml");
    assert_eq!(m.refs.len(), 2);
    let q3 = &m.refs[0];
    assert_eq!(q3.id.as_str(), "r-q3rep");
    assert_eq!(q3.kind, RefKind::Pdf);
    assert_eq!(q3.version, "3f9a1c2b7e01");
    assert!(q3.file.contains(&q3.version));
    assert_eq!(q3.source, RefSource::File);
    assert_eq!(q3.analyst, AnalystOrigin::default());
    assert_eq!(q3.parent, "");
    let fig = &m.refs[1];
    assert_eq!(fig.kind, RefKind::AnalystFigure);
    assert_eq!(fig.source, RefSource::Analyst);
    assert_eq!(fig.analyst.run_id, "20261104-1132-ab12");
    assert_eq!(fig.previous_versions, ["00aa11bb22cc"]);
    assert_eq!(m.get(&fig.id), Some(fig));
    let empty: RefsManifest = toml::from_str("").unwrap();
    assert!(empty.refs.is_empty());
    assert_eq!(toml::to_string(&empty).unwrap(), "");
}

#[test]
fn snapshot_manifest_and_log_round_trip() {
    let m: SnapshotManifest = round_trip_json("snapshot.json");
    assert_eq!(m.snapshot_id.as_str(), "h-0a1b2c3d4e");
    assert_eq!(m.parent.as_ref().unwrap().as_str(), "h-zzzzzzzzz9");
    assert_eq!(m.mode, DeckMode::Structured);
    assert_eq!(m.files[1].path, "deck/slides/s-7f3a.json");
    assert_eq!(m.refs[0].id.as_str(), "r-q3rep");

    let log: Vec<SnapshotLogRecord> = round_trip_jsonl("history_log.jsonl");
    let reasons: Vec<SnapshotReason> = log.iter().map(|r| r.reason).collect();
    assert_eq!(
        reasons,
        [
            SnapshotReason::Manual,
            SnapshotReason::Turn,
            SnapshotReason::ModeSwitch,
            SnapshotReason::Export,
            SnapshotReason::Restore
        ]
    );
    assert_eq!(log[0].parent, None);
    assert_eq!(log[1].turn_id.as_deref(), Some("t-0001"));
    assert_eq!(log[4].label.as_deref(), Some("back to turn 1"));
    let pinned: Vec<bool> = reasons.iter().map(|r| r.is_pinned()).collect();
    assert_eq!(pinned, [false, false, true, true, false]);
    let bad = json!({ "ts": "t", "snapshot_id": "h-Short", "parent": null, "reason": "turn" });
    assert!(serde_json::from_value::<SnapshotLogRecord>(bad).is_err());
}

// ---- transcript ----

#[test]
fn every_transcript_event_round_trips() {
    let events: Vec<Event> = round_trip_jsonl("transcript.jsonl");
    let mut types: Vec<&str> = events.iter().map(Event::type_name).collect();
    for (event, line) in events.iter().zip(fixture("transcript.jsonl").lines()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["type"], json!(event.type_name()));
        assert_eq!(v["ts"], json!(event.ts()));
    }
    types.dedup();
    assert_eq!(
        types,
        [
            "user",
            "assistant",
            "tool_call",
            "tool_result",
            "proposal",
            "decision",
            "run",
            "checkpoint",
            "snapshot",
            "render",
            "check",
            "skill_loaded",
            "export",
            "summary",
            "usage"
        ]
    );
    assert!(matches!(
        &events[6],
        Event::Proposal { kind: ProposalKind::SlideCreate, slides: Some(s), diff: Some(_), .. } if s.len() == 1
    ));
    assert!(matches!(
        &events[8],
        Event::Decision {
            decision: Decision::Accept,
            by: DecidedBy::User,
            ..
        }
    ));
    assert!(
        matches!(&events[11], Event::Checkpoint { snapshot_id, .. } if snapshot_id.as_str() == "h-zzzzzzzzz9")
    );
}

#[test]
fn proposal_kinds_and_decisions() {
    let kinds = [
        "slide-create",
        "slide-update",
        "slide-delete",
        "slide-move",
        "outline",
        "code-edit",
        "build",
        "generated-slide",
        "skill-script",
        "mode-switch",
        "export",
    ];
    for kind in kinds {
        let k: ProposalKind = round_trip_value("proposal kind", &json!(kind));
        assert_eq!(k.is_slide_change(), kind.starts_with("slide-"));
    }
    // Analyst's kinds are not Present's.
    for kind in ["edit", "create", "delete", "package", "shell", "commit"] {
        assert!(serde_json::from_value::<ProposalKind>(json!(kind)).is_err());
    }
    for (d, accepted) in [("accept", true), ("reject", false), ("auto", true)] {
        let d: Decision = round_trip_value("decision", &json!(d));
        assert_eq!(d.is_accepted(), accepted);
    }
    // The Analyst checkpoint shape (`ref`) is not accepted.
    let old = json!({ "ts": "t", "type": "checkpoint", "ref": "refs/analyst/checkpoints/x" });
    assert!(serde_json::from_value::<Event>(old).is_err());
}

// ---- audit ----

#[test]
fn audit_records_round_trip() {
    let records: Vec<AuditRecord> = round_trip_jsonl("audit.jsonl");
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].location, Location::Internal);
    assert_eq!(records[0].refs[0].cite.loc_str(), "p11:c120-188");
    assert_eq!(records[0].refs[1].chars, 410);
    let sources: Vec<AuditImageSource> = records[0].images.iter().map(|i| i.source).collect();
    assert_eq!(
        sources,
        [
            AuditImageSource::Slide,
            AuditImageSource::Ref,
            AuditImageSource::Asset
        ]
    );
    assert_eq!(records[0].status, AuditStatus::Ok);
    assert_eq!(records[0].error, None);
    assert_eq!(records[1].location, Location::External);
    assert_eq!(records[1].status, AuditStatus::Error);
    assert_eq!(records[1].error.as_deref(), Some("connect timeout"));
    assert_eq!(records[2].status, AuditStatus::Refused);
}

// ---- evals ----

#[test]
fn task_file_round_trips_with_every_check_kind() {
    let t: TaskFile = round_trip_toml("task.toml");
    assert_eq!(t.task.name, "margin-story");
    assert_eq!(t.task.mode, Mode::Build);
    assert_eq!(t.fixture.dir, "fixture");
    let mut kinds: Vec<&str> = t.checks.iter().map(Check::kind).collect();
    kinds.dedup();
    assert_eq!(
        kinds,
        [
            "opens_clean",
            "checker_clean",
            "slide_count",
            "layout_used",
            "uses_native",
            "citation_coverage",
            "numbers_verified",
            "text_matches",
            "locked_untouched",
            "mode",
            "export_exists",
            "edits_within_scope"
        ]
    );
    assert_eq!(
        t.checks[1],
        Check::CheckerClean {
            max_warnings: Some(2)
        }
    );
    assert_eq!(
        t.checks[2],
        Check::SlideCount {
            min: Some(4),
            max: Some(6),
            exact: None
        }
    );
    assert_eq!(
        t.checks[3],
        Check::SlideCount {
            min: None,
            max: None,
            exact: Some(5)
        }
    );
    assert_eq!(
        t.checks[5],
        Check::UsesNative {
            element: NativeKind::Chart,
            min: Some(1)
        }
    );
    assert_eq!(t.checks[6], Check::CitationCoverage { min: 0.9 });
    assert!(
        matches!(&t.checks[8], Check::TextMatches { slide: Some(s), .. } if s.as_str() == "s-7f3a")
    );
    assert!(
        matches!(&t.checks[9], Check::TextMatches { slide: None, regex } if regex == "(?i)margin")
    );
    assert_eq!(
        t.checks[11],
        Check::Mode {
            mode: DeckMode::Structured
        }
    );
}

#[test]
fn task_file_defaults_and_modes() {
    let t: TaskFile = toml::from_str("[task]\nname = \"n\"\nprompt = \"p\"\n").unwrap();
    assert_eq!(t.task.mode, Mode::Build);
    assert_eq!(t.task.timeout_s, 900);
    assert_eq!(t.task.max_steps, 40);
    assert_eq!(t.fixture, FixtureSection::default());
    assert!(t.checks.is_empty());
    for mode in ["outline", "build", "polish"] {
        let m: Mode = mode.parse().unwrap();
        assert_eq!(m.to_string(), mode);
        assert_eq!(serde_json::to_value(m).unwrap(), json!(mode));
    }
    assert!("plan".parse::<Mode>().is_err());
    for element in ["chart", "table", "diagram"] {
        round_trip_value::<NativeKind>("native kind", &json!(element));
    }
    // Analyst's check kinds are not Present's.
    let analyst = "[task]\nname = \"n\"\nprompt = \"p\"\n[[checks]]\nkind = \"contract_clean\"\n";
    assert!(toml::from_str::<TaskFile>(analyst).is_err());
}

#[test]
fn scorecard_round_trips() {
    let v = json!({
        "model": "p/m",
        "tasks": [{
            "name": "margin-story", "passed": true,
            "checks": [{ "kind": "opens_clean", "passed": true, "detail": "ok" }],
            "steps": 12, "input_tokens": 1000, "output_tokens": 200, "wall_s": 41.5,
            "unwanted_edits": 0, "tool_errors": 1, "tool_calls": 14
        }],
        "summary": { "pass_rate": 1.0, "unwanted_edit_rate": 0.0, "tool_error_rate": 0.5 }
    });
    let sc: Scorecard = round_trip_value("scorecard", &v);
    assert_eq!(sc.tasks[0].checks[0].kind, "opens_clean");
}
