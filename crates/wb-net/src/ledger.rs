//! The egress ledger: one JSON line per network decision, appended and never rewritten.
//! It records where a connection went and why, never what was sent.

use crate::EgressClass;
use std::io::Write;
use std::path::{Path, PathBuf};

/// File name of the ledger inside the app's home directory.
pub const LEDGER_FILE: &str = "egress.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LedgerEntry {
    /// RFC 3339, UTC.
    pub ts: String,
    pub class: EgressClass,
    /// `host:port` for HTTP, the host name for SSH.
    pub destination: String,
    pub allowed: bool,
    /// A short label such as `chat`, `uv add`, `sync`.
    pub purpose: String,
}

impl LedgerEntry {
    pub fn now(class: EgressClass, destination: &str, allowed: bool, purpose: &str) -> LedgerEntry {
        LedgerEntry {
            ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            class,
            destination: destination.to_string(),
            allowed,
            // One line per entry, whatever the caller passed.
            purpose: purpose
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect(),
        }
    }
}

#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub fn new(path: PathBuf) -> Ledger {
        Ledger { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, entry: &LedgerEntry) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        line.push('\n');
        // One write of one line on an append-mode handle: concurrent writers do not interleave.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())
    }
}

/// Read a ledger file. Lines that do not parse are skipped.
pub fn read(path: &Path) -> std::io::Result<Vec<LedgerEntry>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}
