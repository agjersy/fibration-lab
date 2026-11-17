//! Helpers shared by the integration tests.
#![allow(dead_code)]

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn fixture(name: &str) -> String {
    let path = fixture_path(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

pub fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Parses `value` as `T`, serializes it again, and requires the JSON to be unchanged.
pub fn round_trip_value<T: Serialize + DeserializeOwned>(what: &str, value: &Value) -> T {
    let typed: T =
        serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{what}: parse: {e}"));
    let back = serde_json::to_value(&typed).unwrap_or_else(|e| panic!("{what}: serialize: {e}"));
    assert_eq!(&back, value, "{what}: JSON changed in the round trip");
    // A second pass through text, as the files and the wire carry it.
    let text = serde_json::to_string(&typed).unwrap();
    let again: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(&again, value, "{what}: JSON text changed in the round trip");
    typed
}

/// Round-trips a JSON fixture file.
pub fn round_trip_json<T: Serialize + DeserializeOwned>(name: &str) -> T {
    round_trip_value(name, &fixture_json(name))
}

/// Round-trips every line of a JSON Lines fixture.
pub fn round_trip_jsonl<T: Serialize + DeserializeOwned>(name: &str) -> Vec<T> {
    fixture(name)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            let what = format!("{name}:{}", i + 1);
            let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{what}: {e}"));
            round_trip_value(&what, &v)
        })
        .collect()
}

/// Parses a TOML fixture as `T`, serializes it, and requires the TOML data to be unchanged.
pub fn round_trip_toml<T: Serialize + DeserializeOwned>(name: &str) -> T {
    let text = fixture(name);
    let original: toml::Value = toml::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    let typed: T = toml::from_str(&text).unwrap_or_else(|e| panic!("{name}: parse: {e}"));
    let out = toml::to_string(&typed).unwrap_or_else(|e| panic!("{name}: serialize: {e}"));
    let back: toml::Value = toml::from_str(&out).unwrap_or_else(|e| panic!("{name}: reparse: {e}"));
    assert_eq!(
        back, original,
        "{name}: TOML changed in the round trip:\n{out}"
    );
    typed
}
