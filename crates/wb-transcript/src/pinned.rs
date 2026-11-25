// Ported from Fibration Lab `crates/core/src/agent/mod.rs` (the `pinned` list: `pin_fact`,
// the `Declared scope:` and `Plan:` entries, the `PINNED FACTS` system prompt block) at
// commit f229e4d.
// Adapted: a type of its own, persisted as `pinned.json` next to the conversation so the
// facts survive a restart, with size bounds.
//! Pinned facts: short strings (decisions, names, parameter values) the agent maintains
//! through a tool and that stay in the system prompt when older history is compacted.

use crate::util::truncate_bytes;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Longest fact kept (bytes); longer facts are cut.
pub const MAX_FACT_BYTES: usize = 1000;
/// Most facts kept; the oldest are dropped first.
pub const MAX_FACTS: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PinnedFacts {
    #[serde(default)]
    facts: Vec<String>,
}

impl PinnedFacts {
    pub fn new() -> Self {
        PinnedFacts::default()
    }

    /// Load from a `pinned.json`; a missing file is an empty list.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut p: PinnedFacts =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        p.enforce_bounds();
        Ok(p)
    }

    /// Write atomically (temporary file, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
    }

    pub fn facts(&self) -> &[String] {
        &self.facts
    }
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    fn enforce_bounds(&mut self) {
        for f in &mut self.facts {
            if f.len() > MAX_FACT_BYTES {
                *f = truncate_bytes(f, MAX_FACT_BYTES).to_string();
            }
        }
        if self.facts.len() > MAX_FACTS {
            let drop = self.facts.len() - MAX_FACTS;
            self.facts.drain(..drop);
        }
    }

    /// Pin a fact. Blank facts and exact duplicates are ignored; returns whether it was added.
    pub fn add(&mut self, fact: impl AsRef<str>) -> bool {
        let fact = fact.as_ref().trim();
        if fact.is_empty() {
            return false;
        }
        let fact = truncate_bytes(fact, MAX_FACT_BYTES).to_string();
        if self.facts.contains(&fact) {
            return false;
        }
        self.facts.push(fact);
        self.enforce_bounds();
        true
    }

    /// Replace every fact that starts with `prefix` by one fact `<prefix><value>` (Analyst
    /// keeps a single `Declared scope: ...` entry this way).
    pub fn set_prefixed(&mut self, prefix: &str, value: &str) {
        self.remove_prefixed(prefix);
        self.add(format!("{prefix}{value}"));
    }

    /// Remove facts that start with `prefix`; returns how many were removed.
    pub fn remove_prefixed(&mut self, prefix: &str) -> usize {
        let before = self.facts.len();
        self.facts.retain(|f| !f.starts_with(prefix));
        before - self.facts.len()
    }

    /// Remove the fact at a 0-based index.
    pub fn remove(&mut self, index: usize) -> Option<String> {
        (index < self.facts.len()).then(|| self.facts.remove(index))
    }

    pub fn clear(&mut self) {
        self.facts.clear();
    }

    /// The block for the system prompt, or an empty string when there are no facts:
    ///
    /// ```text
    /// PINNED FACTS:
    /// - fact one
    /// - fact two
    /// ```
    pub fn render_block(&self) -> String {
        if self.facts.is_empty() {
            return String::new();
        }
        let mut s = String::from("PINNED FACTS:\n");
        for f in &self.facts {
            s.push_str("- ");
            s.push_str(&f.replace('\n', " "));
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dedupe_prefix_and_render() {
        let mut p = PinnedFacts::new();
        assert_eq!(p.render_block(), "");
        assert!(p.add("Audience: the board"));
        assert!(!p.add("  Audience: the board "));
        assert!(!p.add("   "));
        p.set_prefixed("Declared scope: ", "s-001, s-002");
        p.set_prefixed("Declared scope: ", "s-003");
        assert!(p.add("multi\nline"));
        assert_eq!(
            p.facts(),
            [
                "Audience: the board",
                "Declared scope: s-003",
                "multi\nline"
            ]
        );
        assert_eq!(
            p.render_block(),
            "PINNED FACTS:\n- Audience: the board\n- Declared scope: s-003\n- multi line\n"
        );
        assert_eq!(p.remove(0).as_deref(), Some("Audience: the board"));
        assert_eq!(p.remove(9), None);
        assert_eq!(p.remove_prefixed("Declared scope:"), 1);
        assert_eq!(p.len(), 1);
        p.clear();
        assert!(p.is_empty());
    }

    #[test]
    fn bounds_are_enforced() {
        let mut p = PinnedFacts::new();
        p.add("é".repeat(MAX_FACT_BYTES));
        assert!(p.facts()[0].len() <= MAX_FACT_BYTES);
        for i in 0..MAX_FACTS + 5 {
            p.add(format!("fact {i}"));
        }
        assert_eq!(p.len(), MAX_FACTS);
        assert_eq!(p.facts()[MAX_FACTS - 1], format!("fact {}", MAX_FACTS + 4));
        assert_eq!(p.facts()[0], "fact 5");
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations/c1.pinned.json");
        assert!(PinnedFacts::load(&path).unwrap().is_empty());
        let mut p = PinnedFacts::new();
        p.add("Currency: EUR");
        p.save(&path).unwrap();
        assert_eq!(PinnedFacts::load(&path).unwrap(), p);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"facts": ["Currency: EUR"]}));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "not json").unwrap();
        assert!(PinnedFacts::load(&path).is_err());
    }
}
