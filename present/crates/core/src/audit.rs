//! The local audit log of model requests (PROTOCOL 10): metadata only, never content.

use analyst_deck_protocol::audit::AuditRecord;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct AuditLog {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl AuditLog {
    pub fn new(home: &Path) -> AuditLog {
        AuditLog {
            dir: home.join("audit"),
            lock: Mutex::new(()),
        }
    }

    fn file_for(&self, ts: &str) -> PathBuf {
        // Records carry RFC 3339 timestamps, so the month is the first seven characters.
        let month = ts.get(..7).unwrap_or("unknown");
        self.dir.join(format!("{month}.jsonl"))
    }

    pub fn append(&self, record: &AuditRecord) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(&self.dir)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file_for(&record.ts))?;
        let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        writeln!(file, "{line}")
    }

    /// Records with `from <= ts < to` (RFC 3339 strings compare chronologically in UTC).
    pub fn read(&self, from: &str, to: &str) -> std::io::Result<Vec<AuditRecord>> {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect(),
            Err(_) => return Ok(Vec::new()),
        };
        files.sort();
        let mut out = Vec::new();
        for file in files {
            for line in std::fs::read_to_string(file)?.lines() {
                if let Ok(rec) = serde_json::from_str::<AuditRecord>(line) {
                    if rec.ts.as_str() >= from && rec.ts.as_str() < to {
                        out.push(rec);
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn export_csv(&self, from: &str, to: &str, out: &Path) -> std::io::Result<usize> {
        fn cell(s: &str) -> String {
            if s.contains([',', '"', '\n']) {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        }
        let records = self.read(from, to)?;
        let mut text = String::from("ts,project,conversation,turn,provider,location,host,model,input_tokens,output_tokens,refs,ref_chars,images,status,error\n");
        for r in &records {
            let location = serde_json::to_value(r.location)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let status = serde_json::to_value(r.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let cites: Vec<String> = r.refs.iter().map(|x| x.cite.to_string()).collect();
            let chars: u64 = r.refs.iter().map(|x| x.chars).sum();
            let row = [
                cell(&r.ts),
                cell(&r.project),
                cell(&r.conversation),
                cell(&r.turn),
                cell(&r.provider),
                location,
                cell(&r.base_url_host),
                cell(&r.model),
                r.input_tokens.to_string(),
                r.output_tokens.to_string(),
                cell(&cites.join(" ")),
                chars.to_string(),
                r.images.len().to_string(),
                status,
                cell(r.error.as_deref().unwrap_or("")),
            ];
            text.push_str(&row.join(","));
            text.push('\n');
        }
        crate::util::write_atomic(out, text.as_bytes())?;
        Ok(records.len())
    }
}
