//! `schemas/deck.schema.json` and `schemas/slide.schema.json` must equal what the Rust
//! types generate (PROTOCOL 2). Rewrite them with:
//!
//! ```text
//! PRESENT_UPDATE_SCHEMAS=1 cargo test -p analyst-deck-protocol --test schemas
//! ```

use analyst_deck_protocol::schema::{
    deck_schema_json, slide_schema_json, DECK_SCHEMA_FILE, SLIDE_SCHEMA_FILE,
};
use serde_json::Value;
use std::path::PathBuf;

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
}

fn update_requested() -> bool {
    std::env::var("PRESENT_UPDATE_SCHEMAS")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Writes the file when updating; otherwise fails if the committed file differs.
fn check_or_update(file: &str, generated: &str) {
    let path = schemas_dir().join(file);
    if update_requested() {
        std::fs::create_dir_all(schemas_dir()).unwrap();
        std::fs::write(&path, generated)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}\nrun `PRESENT_UPDATE_SCHEMAS=1 cargo test -p analyst-deck-protocol --test schemas` to create it",
            path.display()
        )
    });
    // A checkout may convert line endings; nothing else is tolerated.
    let committed = committed.replace("\r\n", "\n");
    assert!(
        committed == generated,
        "schemas/{file} is out of date with crates/protocol.\n\
         Run `PRESENT_UPDATE_SCHEMAS=1 cargo test -p analyst-deck-protocol --test schemas` and commit the result.\n\
         First difference at line {}.",
        first_difference(&committed, generated)
    );
}

fn first_difference(a: &str, b: &str) -> usize {
    a.lines()
        .zip(b.lines())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.lines().count().min(b.lines().count()))
        + 1
}

#[test]
fn deck_schema_is_up_to_date() {
    check_or_update(DECK_SCHEMA_FILE, &deck_schema_json());
}

#[test]
fn slide_schema_is_up_to_date() {
    check_or_update(SLIDE_SCHEMA_FILE, &slide_schema_json());
}

#[test]
fn generation_is_deterministic() {
    assert_eq!(deck_schema_json(), deck_schema_json());
    assert_eq!(slide_schema_json(), slide_schema_json());
    assert!(slide_schema_json().ends_with("}\n"));
}

fn slide_schema() -> Value {
    serde_json::from_str(&slide_schema_json()).unwrap()
}

fn required(schema: &Value) -> Vec<&str> {
    let mut r: Vec<&str> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    r.sort_unstable();
    r
}

/// The variants of an internally tagged enum, keyed by the tag's value.
fn variant<'a>(schema: &'a Value, tag: &str, value: &str) -> &'a Value {
    schema["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("no oneOf in {schema}"))
        .iter()
        .find(|v| v["properties"][tag]["enum"] == serde_json::json!([value]))
        .unwrap_or_else(|| panic!("no variant {tag}={value}"))
}

#[test]
fn slide_schema_matches_protocol_2_2() {
    let s = slide_schema();
    assert_eq!(s["$schema"], "http://json-schema.org/draft-07/schema#");
    assert_eq!(s["title"], "Slide");
    assert_eq!(s["type"], "object");
    assert_eq!(s["additionalProperties"], false);
    // `kind` defaults to "layout" and is therefore not required.
    assert_eq!(required(&s), ["elements", "id", "layout", "placeholders"]);
    assert_eq!(s["properties"]["kind"]["default"], "layout");
    let defs = &s["definitions"];
    assert_eq!(defs["SlideId"]["pattern"], "^s-[0-9a-z][0-9a-z-]{0,23}$");
    assert_eq!(
        defs["ElementId"]["pattern"],
        "^e-[0-9A-Za-z][0-9A-Za-z-]{0,31}$"
    );
    assert!(defs["Cite"]["pattern"]
        .as_str()
        .unwrap()
        .starts_with("^ref:r-"));
    assert_eq!(
        defs["SlideKind"]["enum"],
        serde_json::json!(["layout", "generated"])
    );
    // Optional fields are absent, never null.
    assert_eq!(
        s["properties"]["locked"],
        serde_json::json!({ "type": "boolean" })
    );
    // The only `null` in the schema is a table cell value.
    assert_eq!(slide_schema_json().matches("\"null\"").count(), 1);
    assert_eq!(
        defs["CellValue"]["anyOf"][0],
        serde_json::json!({ "type": "null" })
    );
    assert!(!slide_schema_json().contains("\"format\""));

    let content = &defs["PlaceholderContent"];
    for t in [
        "text", "image", "chart", "table", "diagram", "equation", "empty",
    ] {
        let v = variant(content, "type", t);
        assert_eq!(v["additionalProperties"], false, "{t}");
    }
    assert_eq!(
        required(variant(content, "type", "image")),
        ["alt", "asset", "id", "type"]
    );
    assert_eq!(required(variant(content, "type", "empty")), ["type"]);
    let element = &defs["ElementContent"];
    assert_eq!(element["oneOf"].as_array().unwrap().len(), 7);
    assert_eq!(
        required(variant(content, "type", "equation")),
        ["eq_cite", "id", "type"]
    );
    assert_eq!(
        required(&defs["EqCite"]),
        ["check", "latex", "path", "qualname", "source_hash", "tier"]
    );
    assert_eq!(
        defs["EqTier"]["enum"],
        serde_json::json!(["derived", "declared", "latex"])
    );
    assert_eq!(
        required(variant(element, "type", "shape")),
        ["id", "shape", "type"]
    );

    let level = &defs["Paragraph"]["properties"]["level"];
    assert_eq!(
        (level["minimum"].as_f64(), level["maximum"].as_f64()),
        (Some(0.0), Some(4.0))
    );
    assert_eq!(required(&defs["Paragraph"]), ["runs"]);
    assert_eq!(required(&defs["Run"]), ["t"]);
    assert_eq!(
        defs["ThemeColor"]["enum"],
        serde_json::json!([
            "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5",
            "accent6", "hlink", "folHlink"
        ])
    );

    let diagram = &defs["Diagram"];
    for family in ["process", "timeline", "matrix", "hierarchy"] {
        variant(diagram, "family", family);
    }
    assert_eq!(
        required(variant(diagram, "family", "matrix")),
        ["cells", "family", "variant"]
    );
    let step = &defs["TransformStep"];
    for op in [
        "select",
        "rename",
        "filter",
        "sort",
        "top_n",
        "group_sum",
        "group_mean",
        "group_count",
        "pivot",
        "unpivot",
        "pct_change",
        "share_of_total",
        "cumulative",
        "scale",
        "round",
    ] {
        variant(step, "op", op);
    }
    assert_eq!(step["oneOf"].as_array().unwrap().len(), 15);
    assert!(variant(step, "op", "pct_change")["properties"]
        .get("as")
        .is_some());
    let source = &defs["ChartSource"];
    assert_eq!(required(variant(source, "kind", "ref")), ["cite", "kind"]);
    assert_eq!(
        required(variant(source, "kind", "inline")),
        ["cites", "kind", "table"]
    );
    assert_eq!(required(&defs["Chart"]), ["series_from", "source", "type"]);
}

#[test]
fn deck_schema_matches_protocol_2_1() {
    let d: Value = serde_json::from_str(&deck_schema_json()).unwrap();
    assert_eq!(d["title"], "Deck");
    assert_eq!(d["additionalProperties"], false);
    assert_eq!(
        required(&d),
        ["mode", "schema_version", "sections", "template", "title"]
    );
    assert_eq!(d["properties"]["schema_version"]["const"], 1);
    assert_eq!(
        d["definitions"]["DeckMode"]["enum"],
        serde_json::json!(["structured", "code"])
    );
    assert_eq!(required(&d["definitions"]["Section"]), ["slides", "title"]);
    assert!(d["$id"].as_str().unwrap().ends_with("/deck.schema.json"));
}
