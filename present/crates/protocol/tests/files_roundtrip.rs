//! Round trips of the template description and sidecar (PROTOCOL 3, 6.4), the
//! configuration and project files (4), transcript events (6.2), the audit record (10),
//! and the evaluation task file (12).

mod common;

use common::{round_trip_json, round_trip_toml, round_trip_value};
use fibration_deck_protocol::config::*;
use fibration_deck_protocol::deck::{DeckMode, ThemeColor};
use fibration_deck_protocol::evals::*;
use fibration_deck_protocol::kernel::{RefKind, Severity};
use fibration_deck_protocol::template::*;
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
    // The `mcp` key above comes from an older file: it still loads and is dropped.
    assert!(!toml::to_string(&p).unwrap().contains("mcp"));
    let again: ProjectConfig = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
    assert_eq!(again, p);
    // `preset` expands to the detailed keys, which still override.
    let ro: ProjectConfig = toml::from_str("[policy]\npreset = \"read-only\"\n").unwrap();
    assert_eq!(ro.policy.preset, Some(PolicyPreset::ReadOnly));
    assert_eq!(ro.policy.edits, EditPolicy::Deny);
    assert_eq!(ro.policy.code, Gate::Auto);
    assert_eq!(ro.policy.code_mode, Permission::Deny);
    let over: ProjectConfig =
        toml::from_str("[policy]\npreset = \"review\"\ncode = \"deny\"\n").unwrap();
    assert_eq!(
        (over.policy.code, over.policy.skills_scripts),
        (Gate::Deny, Gate::Auto)
    );
    let again: ProjectConfig = toml::from_str(&toml::to_string(&over).unwrap()).unwrap();
    assert_eq!(again, over);
    for edits in ["review", "auto-new-slides", "auto", "deny"] {
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

// ---- transcript ----

// ---- audit ----

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
    let fibration = "[task]\nname = \"n\"\nprompt = \"p\"\n[[checks]]\nkind = \"contract_clean\"\n";
    assert!(toml::from_str::<TaskFile>(fibration).is_err());
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
