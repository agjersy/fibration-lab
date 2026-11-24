//! JSON Schema generation for the deck document (PROTOCOL 2).
//!
//! `schemas/deck.schema.json` and `schemas/slide.schema.json` are the output of
//! [`deck_schema_json`] and [`slide_schema_json`], committed to the repository. The test
//! `tests/schemas.rs` fails when they drift; run it with `PRESENT_UPDATE_SCHEMAS=1` to
//! rewrite them. Output is draft-07 with object keys sorted, so it does not depend on
//! which `serde_json` features the rest of the workspace enables.
//!
//! The schema is the structural contract. Two things the Rust types check on parse are
//! only approximated in it: a `Cite` is matched by a loose pattern (the location grammar
//! is not a regular expression worth publishing), and the Rust side does not enforce the
//! numeric ranges or the asset path pattern that the schema states.

use schemars::gen::{SchemaGenerator, SchemaSettings};
use schemars::schema::{InstanceType, RootSchema, Schema, SchemaObject, StringValidation};
use schemars::JsonSchema;
use serde_json::Value;

/// `$id` prefix of the published schemas.
pub const SCHEMA_ID_BASE: &str = "https://personal-present.invalid/schemas";

fn root_schema<T: JsonSchema>() -> RootSchema {
    // Optional fields are absent, never null, as in the TypeScript of PROTOCOL 2.
    let settings = SchemaSettings::draft07().with(|s| s.option_add_null_type = false);
    SchemaGenerator::new(settings).into_root_schema_for::<T>()
}

/// schemars' Rust-specific numeric `format` annotations; not JSON Schema formats.
const RUST_NUMBER_FORMATS: [&str; 12] = [
    "int8", "int16", "int32", "int64", "uint8", "uint16", "uint32", "uint64", "uint", "int",
    "float", "double",
];

/// Rebuilds every object with its keys in sorted order and drops the Rust-specific
/// numeric `format` annotations (the range they imply is already stated as `minimum`).
fn normalize(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .filter(|(k, v)| {
                    !(k == "format"
                        && v.as_str()
                            .map(|f| RUST_NUMBER_FORMATS.contains(&f))
                            .unwrap_or(false))
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, normalize(v)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        other => other,
    }
}

fn render<T: JsonSchema>(file: &str) -> String {
    let mut v = serde_json::to_value(root_schema::<T>()).expect("a schema is plain JSON");
    if let Value::Object(map) = &mut v {
        map.insert(
            "$id".to_string(),
            Value::String(format!("{SCHEMA_ID_BASE}/{file}")),
        );
    }
    let mut text = serde_json::to_string_pretty(&normalize(v)).expect("a schema is plain JSON");
    text.push('\n');
    text
}

/// File name of the deck schema under `schemas/`.
pub const DECK_SCHEMA_FILE: &str = "deck.schema.json";
/// File name of the slide schema under `schemas/`.
pub const SLIDE_SCHEMA_FILE: &str = "slide.schema.json";

/// The text of `schemas/deck.schema.json`.
pub fn deck_schema_json() -> String {
    render::<crate::deck::Deck>(DECK_SCHEMA_FILE)
}

/// The text of `schemas/slide.schema.json`.
pub fn slide_schema_json() -> String {
    render::<crate::deck::Slide>(SLIDE_SCHEMA_FILE)
}

/// `{"type": "string", "pattern": <pattern>}`.
pub(crate) fn string_with_pattern(pattern: &str) -> Schema {
    SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        string: Some(Box::new(StringValidation {
            pattern: Some(pattern.to_string()),
            ..Default::default()
        })),
        ..Default::default()
    }
    .into()
}

/// `{"type": "integer", "const": 1}` for `Deck.schema_version`.
pub(crate) fn const_schema_version(_: &mut SchemaGenerator) -> Schema {
    SchemaObject {
        instance_type: Some(InstanceType::Integer.into()),
        const_value: Some(Value::from(crate::SCHEMA_VERSION)),
        ..Default::default()
    }
    .into()
}
