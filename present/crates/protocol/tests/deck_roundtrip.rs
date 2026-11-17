//! Round trips of the deck document fixtures (PROTOCOL 2) and the rejections the types
//! enforce.

mod common;

use common::{fixture_json, round_trip_json};
use present_protocol::deck::*;
use serde_json::{json, Value};

const SLIDE_FIXTURES: [&str; 9] = [
    "slide_layout.json",
    "slide_chart.json",
    "slide_table.json",
    "slide_diagram_process.json",
    "slide_diagram_timeline.json",
    "slide_diagram_matrix.json",
    "slide_diagram_hierarchy.json",
    "slide_free_elements.json",
    "slide_generated.json",
];

#[test]
fn every_slide_fixture_round_trips() {
    for name in SLIDE_FIXTURES {
        let slide: Slide = round_trip_json(name);
        // Clone and compare exercises PartialEq on the whole tree.
        assert_eq!(slide.clone(), slide, "{name}");
    }
}

#[test]
fn layout_slide_content() {
    let s: Slide = round_trip_json("slide_layout.json");
    assert_eq!(s.id.as_str(), "s-7f3a");
    assert_eq!(s.kind, SlideKind::Layout);
    assert_eq!(s.layout, "Title and Content");
    assert_eq!(s.locked, Some(false));
    assert_eq!(s.placeholders.len(), 3);
    assert_eq!(s.placeholders["idx:14"], PlaceholderContent::Empty);
    assert_eq!(s.placeholders["idx:14"].id(), None);
    let PlaceholderContent::Text { id, text } = &s.placeholders["body"] else {
        panic!("body is not text");
    };
    assert_eq!(id.as_str(), "e-b1a0");
    assert_eq!(text.paragraphs[1].level, Some(1));
    assert_eq!(
        text.to_plain(),
        "Gross margin up 3.1 pts quarter on quarter\nTwo thirds from list-price changes in EMEA\nSee the pricing memo"
    );
    let cites: Vec<&str> = text.cites().map(|c| c.as_str()).collect();
    assert_eq!(
        cites,
        [
            "ref:r-q3rep@3f9a1c2b7e01#p11:c120-188",
            "ref:r-q3rep@3f9a1c2b7e01#p12"
        ]
    );
    // `cite: []` (non-factual) is kept distinct from an absent `cite`.
    let third = &text.paragraphs[2];
    assert_eq!(third.runs[0].cite, Some(vec![]));
    assert_eq!(third.runs[1].cite, None);
    assert_eq!(third.runs[1].color, Some(ThemeColor::Accent2));
    assert_eq!(third.bullet, Some(Bullet::None));
    assert_eq!(
        s.notes.unwrap().to_plain(),
        "Pause on the EMEA point; Sarah owns the follow-up."
    );
}

#[test]
fn chart_slide_covers_every_transform_op() {
    let s: Slide = round_trip_json("slide_chart.json");
    let PlaceholderContent::Chart { chart, .. } = &s.placeholders["body"] else {
        panic!("body is not a chart");
    };
    assert_eq!(chart.r#type, ChartType::Combo);
    let ChartSource::Ref { cite } = &chart.source else {
        panic!("source is not a ref");
    };
    assert_eq!(cite.loc_str(), "Pipeline!A1:E9");
    let mut ops: Vec<&str> = chart
        .steps
        .as_ref()
        .unwrap()
        .iter()
        .map(|s| s.op())
        .collect();
    ops.sort_unstable();
    ops.dedup();
    assert_eq!(
        ops,
        [
            "cumulative",
            "filter",
            "group_count",
            "group_mean",
            "group_sum",
            "pct_change",
            "pivot",
            "rename",
            "round",
            "scale",
            "select",
            "share_of_total",
            "sort",
            "top_n",
            "unpivot"
        ]
    );
    let computed = chart.computed.as_ref().unwrap();
    assert_eq!(computed.rows[0][0].as_str(), Some("EMEA"));
    assert_eq!(computed.rows[0][1].as_f64(), Some(412.5));
    assert_eq!(computed.rows[1][1], CellValue::from(310));
    assert_eq!(computed.rows[3][1], CellValue::Null);
}

#[test]
fn every_filter_comparison_round_trips() {
    for cmp in ["==", "!=", "<", "<=", ">", ">=", "in", "not_in", "contains"] {
        let v = json!({ "op": "filter", "column": "c", "cmp": cmp, "value": "x" });
        let step: TransformStep = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&step).unwrap(), v);
    }
    let bad = json!({ "op": "filter", "column": "c", "cmp": "=", "value": 1 });
    assert!(serde_json::from_value::<TransformStep>(bad).is_err());
    let unknown_op = json!({ "op": "eval", "code": "1+1" });
    assert!(serde_json::from_value::<TransformStep>(unknown_op).is_err());
}

#[test]
fn diagram_families() {
    let family = |name: &str, key: &str| -> Diagram {
        let s: Slide = round_trip_json(name);
        match &s.placeholders[key] {
            PlaceholderContent::Diagram { diagram, .. } => diagram.clone(),
            other => panic!("{name}: {other:?}"),
        }
    };
    assert!(matches!(
        family("slide_diagram_process.json", "body"),
        Diagram::Process { variant: ProcessVariant::Flowchart, ref nodes, edges: Some(ref e) }
            if nodes.len() == 4 && e.len() == 4 && nodes[1].kind == Some(DNodeKind::Decision)
    ));
    assert!(matches!(
        family("slide_diagram_timeline.json", "body"),
        Diagram::Timeline { variant: TimelineVariant::Swimlanes, ref items, axis: Some(ref a), .. }
            if items.len() == 3 && a.unit == TimeUnit::Quarter
    ));
    assert!(matches!(
        family("slide_diagram_matrix.json", "body"),
        Diagram::Matrix {
            variant: MatrixVariant::TwoByTwo,
            axes: Some(_),
            ..
        }
    ));
    assert!(matches!(
        family("slide_diagram_matrix.json", "body-2"),
        Diagram::Matrix { variant: MatrixVariant::Grid, ref cells, .. } if cells[0].score == Some(4)
    ));
    assert!(matches!(
        family("slide_diagram_matrix.json", "idx:20"),
        Diagram::Matrix {
            variant: MatrixVariant::KpiTiles,
            ..
        }
    ));
    assert!(matches!(
        family("slide_diagram_hierarchy.json", "body"),
        Diagram::Hierarchy { variant: HierarchyVariant::DriverTree, ref nodes }
            if nodes[0].parent.is_none() && nodes[1].parent.as_deref() == Some("root")
    ));
    // Every variant name of every family.
    for (fam, variants, rest) in [
        (
            "process",
            &["chevrons", "steps", "flowchart"][..],
            json!({ "nodes": [] }),
        ),
        (
            "timeline",
            &["milestones", "swimlanes", "gantt"][..],
            json!({ "items": [] }),
        ),
        (
            "matrix",
            &["2x2", "grid", "kpi-tiles"][..],
            json!({ "cells": [] }),
        ),
        (
            "hierarchy",
            &["org", "tree", "driver-tree"][..],
            json!({ "nodes": [] }),
        ),
    ] {
        for variant in variants {
            let mut v = rest.clone();
            v["family"] = json!(fam);
            v["variant"] = json!(variant);
            let d: Diagram = serde_json::from_value(v.clone()).unwrap();
            assert_eq!(serde_json::to_value(&d).unwrap(), v);
        }
    }
}

#[test]
fn free_elements_cover_every_content_type() {
    let s: Slide = round_trip_json("slide_free_elements.json");
    let types: Vec<Value> = fixture_json("slide_free_elements.json")["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["content"]["type"].clone())
        .collect();
    assert_eq!(
        types,
        ["shape", "shape", "text", "image", "chart", "table", "diagram"]
    );
    for el in &s.elements {
        assert_eq!(&el.id, el.content.id());
    }
    assert!(s.elements[0]
        .content
        .clone()
        .into_placeholder_content()
        .is_none());
    assert!(matches!(
        s.elements[2].content.clone().into_placeholder_content(),
        Some(PlaceholderContent::Text { .. })
    ));
    // A free element cannot be `empty`.
    let empty =
        json!({ "id": "e-aaaa", "x": 0, "y": 0, "cx": 1, "cy": 1, "content": { "type": "empty" } });
    assert!(serde_json::from_value::<FreeElement>(empty).is_err());
    // A placeholder cannot hold a bare shape.
    let shape = json!({ "type": "shape", "id": "e-aaaa", "shape": "rect" });
    assert!(serde_json::from_value::<PlaceholderContent>(shape.clone()).is_err());
    for kind in ["rect", "rounded-rect", "ellipse", "line", "arrow"] {
        let mut v = shape.clone();
        v["shape"] = json!(kind);
        let c: ElementContent = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&c).unwrap(), v);
    }
}

#[test]
fn generated_slide() {
    let s: Slide = round_trip_json("slide_generated.json");
    assert_eq!(s.kind, SlideKind::Generated);
    assert!(s.generated.unwrap().script.contains("present_sdk"));
    assert_eq!(s.hidden, Some(true));
}

#[test]
fn deck_round_trips() {
    let d: Deck = round_trip_json("deck.json");
    assert_eq!(d.schema_version, present_protocol::SCHEMA_VERSION);
    assert_eq!(d.mode, DeckMode::Structured);
    assert_eq!(d.slide_ids().count(), 9);
    assert_eq!(
        d.slide_size,
        Some(Size {
            cx: 12_192_000,
            cy: 6_858_000
        })
    );
    // The deck lists exactly the slide fixtures.
    let mut listed: Vec<String> = d.slide_ids().map(|s| s.to_string()).collect();
    let mut present: Vec<String> = SLIDE_FIXTURES
        .iter()
        .map(|f| fixture_json(f)["id"].as_str().unwrap().to_string())
        .collect();
    listed.sort();
    present.sort();
    assert_eq!(listed, present);

    let minimal: Deck = round_trip_json("deck_minimal.json");
    assert_eq!(minimal.mode, DeckMode::Code);
    assert_eq!(minimal.author, None);
    assert_eq!(minimal.mode.to_string(), "code");
}

#[test]
fn theme_colors_use_the_ooxml_names() {
    let names = [
        "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink", "folHlink",
    ];
    for (color, name) in ThemeColor::ALL.iter().zip(names) {
        assert_eq!(serde_json::to_value(color).unwrap(), json!(name));
        assert_eq!(
            &serde_json::from_value::<ThemeColor>(json!(name)).unwrap(),
            color
        );
    }
    assert!(serde_json::from_value::<ThemeColor>(json!("#FF0000")).is_err());
    assert!(serde_json::from_value::<ThemeColor>(json!("Accent1")).is_err());
}

#[test]
fn documents_reject_what_the_contract_forbids() {
    let base = fixture_json("slide_layout.json");
    let reject = |what: &str, edit: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        edit(&mut v);
        assert!(
            serde_json::from_value::<Slide>(v).is_err(),
            "{what} was accepted"
        );
    };
    reject("unknown slide field", &|v| v["colour"] = json!("red"));
    reject("unknown run field", &|v| {
        v["placeholders"]["title"]["text"]["paragraphs"][0]["runs"][0]["bold"] = json!(true)
    });
    reject("unknown placeholder field", &|v| {
        v["placeholders"]["title"]["font"] = json!("Arial")
    });
    reject("unknown content type", &|v| {
        v["placeholders"]["title"]["type"] = json!("video")
    });
    reject("text content without an element id", &|v| {
        v["placeholders"]["title"]
            .as_object_mut()
            .unwrap()
            .remove("id");
    });
    reject("bad slide id", &|v| v["id"] = json!("slide-1"));
    reject("bad element id", &|v| {
        v["placeholders"]["title"]["id"] = json!("title")
    });
    reject("unversioned citation", &|v| {
        v["placeholders"]["body"]["text"]["paragraphs"][0]["runs"][0]["cite"] =
            json!(["ref:q3-report#p11:c120-188"])
    });
    reject("hex color", &|v| {
        v["placeholders"]["title"]["text"]["paragraphs"][0]["runs"][0]["color"] = json!("FF0000")
    });
    reject("missing layout", &|v| {
        v.as_object_mut().unwrap().remove("layout");
    });
    reject("missing elements", &|v| {
        v.as_object_mut().unwrap().remove("elements");
    });
    reject("unknown slide kind", &|v| v["kind"] = json!("code"));
    reject("element id with an underscore", &|v| {
        v["placeholders"]["title"]["id"] = json!("e-a_b")
    });

    let mut deck = fixture_json("deck.json");
    deck["sections"][0]["slides"][0] = json!("7f3a");
    assert!(serde_json::from_value::<Deck>(deck).is_err());
    let mut deck = fixture_json("deck.json");
    deck["mode"] = json!("hybrid");
    assert!(serde_json::from_value::<Deck>(deck).is_err());
}

#[test]
fn text_helpers() {
    let t = Text::plain("one\ntwo");
    assert_eq!(t.paragraphs.len(), 2);
    assert_eq!(t.to_plain(), "one\ntwo");
    assert_eq!(
        serde_json::to_value(&t).unwrap(),
        json!({ "paragraphs": [{ "runs": [{ "t": "one" }] }, { "runs": [{ "t": "two" }] }] })
    );
    assert_eq!(CellValue::from(f64::NAN), CellValue::Null);
    assert_eq!(CellValue::from(1.5).as_f64(), Some(1.5));
    assert_eq!(CellValue::from("a").as_str(), Some("a"));
}

#[test]
fn slide_kind_defaults_to_layout() {
    let mut v = fixture_json("slide_layout.json");
    v.as_object_mut().unwrap().remove("kind");
    let s: Slide = serde_json::from_value(v).unwrap();
    assert_eq!(s.kind, SlideKind::Layout);
    // It is always written.
    assert_eq!(serde_json::to_value(&s).unwrap()["kind"], json!("layout"));
}

#[test]
fn sdk_element_ids_are_accepted() {
    for id in ["e-raw-1", "e-ftr", "e-sldNum", "e-b2"] {
        let v = json!({ "type": "text", "id": id, "text": { "paragraphs": [] } });
        let c: PlaceholderContent = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&c).unwrap(), v);
    }
}

/// Slides written by the kernel's test corpus (`python/kernel/tests/corpus.py`), copied
/// verbatim: a text slide with notes, a combo chart, a table, a picture, free elements.
const CORPUS_FIXTURES: [&str; 5] = [
    "corpus/s-c002.json",
    "corpus/s-c012.json",
    "corpus/s-c013.json",
    "corpus/s-c016.json",
    "corpus/s-c022.json",
];

#[test]
fn kernel_corpus_slides_round_trip() {
    let mut types = std::collections::BTreeSet::new();
    for name in CORPUS_FIXTURES {
        let slide: Slide = round_trip_json(name);
        for content in slide.placeholders.values() {
            types.insert(
                serde_json::to_value(content).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        for el in &slide.elements {
            assert_eq!(&el.id, el.content.id(), "{name}");
        }
    }
    for t in ["text", "chart", "table", "image"] {
        assert!(types.contains(t), "no corpus fixture with {t} content");
    }
    let free: Slide = round_trip_json("corpus/s-c022.json");
    assert!(!free.elements.is_empty());
    let deck: Deck = round_trip_json("corpus/deck.json");
    assert_eq!(deck.slide_ids().count(), 32);
}

/// With `PRESENT_CORPUS_DIR` pointing at a project made by the kernel's
/// `corpus.make_project`, every slide of it must round-trip. Skipped otherwise.
#[test]
fn kernel_corpus_directory_round_trips_when_given() {
    let Ok(dir) = std::env::var("PRESENT_CORPUS_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir).join("deck");
    let read = |p: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    let deck: Deck = common::round_trip_value("corpus deck.json", &read(&dir.join("deck.json")));
    let mut count = 0;
    for id in deck.slide_ids() {
        let path = dir.join("slides").join(format!("{id}.json"));
        let slide: Slide = common::round_trip_value(&path.display().to_string(), &read(&path));
        assert_eq!(&slide.id, id);
        count += 1;
    }
    assert!(count > 0);
    eprintln!("round-tripped {count} corpus slides");
}
