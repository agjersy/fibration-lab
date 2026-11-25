// Ported from Fibration Lab `crates/core/src/tools/dispatch.rs` (`cap_result`) and
// `crates/core/src/session.rs` (`spill_artifact`) at commit f229e4d.
// Adapted: independent of the tool context; the capped value is guaranteed to fit the cap
// after JSON escaping; artifact names are sanitized.
//! The tool result cap (Analyst PROTOCOL 6, Present PROTOCOL 6.3): a result larger than
//! 16 KiB is stored in full under an artifacts directory and the model receives the head
//! plus `{truncated: true, artifact: "<path>"}`.

use crate::util::truncate_bytes;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Cap on a tool result in the model context, in bytes of its compact JSON.
pub const TOOL_RESULT_CAP_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultCap {
    pub max_bytes: usize,
    /// Where full results are written. `None`: truncate without keeping the full result.
    pub artifacts_dir: Option<PathBuf>,
    /// Sentence added to a truncated result telling the model how to page the artifact
    /// (the tool to use is app-specific).
    pub hint: Option<String>,
}

impl Default for ResultCap {
    fn default() -> Self {
        ResultCap {
            max_bytes: TOOL_RESULT_CAP_BYTES,
            artifacts_dir: None,
            hint: None,
        }
    }
}

/// A result ready for the model context and the transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct Capped {
    /// The original value, or `{head, truncated: true, artifact?, hint?, error?}`.
    pub value: Value,
    pub truncated: bool,
    /// The file holding the full result (pretty-printed JSON), when one was written.
    pub artifact: Option<PathBuf>,
}

impl ResultCap {
    pub fn new(artifacts_dir: impl Into<PathBuf>) -> Self {
        ResultCap {
            artifacts_dir: Some(artifacts_dir.into()),
            ..Default::default()
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Cap a result. `name` is the artifact name relative to the artifacts directory,
    /// without extension (`<conversation id>/<call id>`); `.json` is appended.
    pub fn cap(&self, name: &str, value: Value) -> Capped {
        let text = serde_json::to_string(&value).unwrap_or_default();
        if text.len() <= self.max_bytes {
            return Capped {
                value,
                truncated: false,
                artifact: None,
            };
        }
        let pretty = serde_json::to_string_pretty(&value).unwrap_or(text);
        let (artifact, error) = match &self.artifacts_dir {
            None => (None, None),
            Some(dir) => match spill(dir, name, pretty.as_bytes()) {
                Ok(p) => (Some(p), None),
                Err(e) => (None, Some(format!("could not store full result: {e:#}"))),
            },
        };
        let build = |head: &str| {
            let mut v = json!({"head": head, "truncated": true});
            if let Some(p) = &artifact {
                v["artifact"] = json!(p.display().to_string());
                if let Some(h) = &self.hint {
                    v["hint"] = json!(h);
                }
            }
            if let Some(e) = &error {
                v["error"] = json!(e);
            }
            v
        };
        // Escaping can make the head longer in JSON than in bytes: shrink until it fits.
        let mut head_len = self.max_bytes * 3 / 4;
        let capped = loop {
            let v = build(truncate_bytes(&pretty, head_len));
            let size = serde_json::to_string(&v).map(|s| s.len()).unwrap_or(0);
            if size <= self.max_bytes || head_len == 0 {
                break v;
            }
            head_len = head_len * 3 / 4;
        };
        Capped {
            value: capped,
            truncated: true,
            artifact,
        }
    }
}

fn artifact_rel_path(name: &str) -> Result<PathBuf> {
    let mut rel = PathBuf::new();
    for part in name.split(['/', '\\']) {
        let clean: String = part
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if clean.is_empty() || clean.chars().all(|c| c == '.') {
            bail!("invalid artifact name `{name}`");
        }
        rel.push(clean);
    }
    let file = rel
        .file_name()
        .map(|f| format!("{}.json", f.to_string_lossy()))
        .with_context(|| format!("invalid artifact name `{name}`"))?;
    rel.set_file_name(file);
    Ok(rel)
}

fn spill(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let path = dir.join(artifact_rel_path(name)?);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
    }
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_results_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        let cap = ResultCap::new(dir.path());
        let v = json!({"content": "x".repeat(1000)});
        let c = cap.cap("c1/t1", v.clone());
        assert_eq!(c.value, v);
        assert!(!c.truncated && c.artifact.is_none());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn large_results_spill_and_keep_the_head() {
        let dir = tempfile::tempdir().unwrap();
        let cap = ResultCap::new(dir.path()).with_hint("page it with code.read");
        let v = json!({"content": "x".repeat(40_000), "tail": "END"});
        let c = cap.cap("c1/t1", v.clone());
        assert!(c.truncated);
        assert_eq!(c.value["truncated"], true);
        let artifact = c.artifact.clone().unwrap();
        assert_eq!(artifact, dir.path().join("c1").join("t1.json"));
        assert_eq!(c.value["artifact"], artifact.display().to_string());
        assert_eq!(c.value["hint"], "page it with code.read");
        let head = c.value["head"].as_str().unwrap();
        assert_eq!(head.len(), TOOL_RESULT_CAP_BYTES * 3 / 4);
        assert!(head.starts_with("{\n  \"content\": \"xxx"));
        assert!(serde_json::to_string(&c.value).unwrap().len() <= TOOL_RESULT_CAP_BYTES);
        // The artifact holds the full result.
        let full: Value =
            serde_json::from_str(&std::fs::read_to_string(&artifact).unwrap()).unwrap();
        assert_eq!(full, v);
    }

    #[test]
    fn capped_value_fits_even_when_escaping_inflates_it() {
        let cap = ResultCap::default().with_max_bytes(2048);
        // Quotes and newlines double in size when the head is embedded as a JSON string.
        let v = json!({"rows": vec!["\"quoted\"\n"; 2000]});
        let c = cap.cap("x", v);
        assert!(c.truncated);
        assert!(c.artifact.is_none());
        assert!(c.value.get("artifact").is_none());
        let size = serde_json::to_string(&c.value).unwrap().len();
        assert!(size <= 2048, "{size}");
        assert!(!c.value["head"].as_str().unwrap().is_empty());
    }

    #[test]
    fn exactly_at_the_cap_is_not_truncated() {
        let cap = ResultCap::default().with_max_bytes(100);
        let v = json!("a".repeat(98)); // 98 chars + 2 quotes
        assert!(!cap.cap("x", v).truncated);
        assert!(cap.cap("x", json!("a".repeat(99))).truncated);
    }

    #[test]
    fn artifact_names_cannot_escape_and_failures_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let cap = ResultCap::new(dir.path()).with_max_bytes(64);
        let big = json!("y".repeat(500));
        let c = cap.cap("conv 1/call:7", big.clone());
        assert_eq!(
            c.artifact.unwrap(),
            dir.path().join("conv_1").join("call_7.json")
        );
        let c = cap.cap("../../etc/passwd", big.clone());
        assert!(c.truncated && c.artifact.is_none());
        assert!(c.value["error"]
            .as_str()
            .unwrap()
            .contains("could not store full result"));
        assert!(c.value.get("artifact").is_none());
        assert!(!dir.path().join("../../etc/passwd.json").exists());
    }
}
