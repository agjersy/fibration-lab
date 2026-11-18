//! Append-only, content-addressed snapshot store in `.present/history/` (PROTOCOL 4.4).

use crate::deck_store::DeckStore;
use crate::util::{new_id, now_iso, sha256_hex, to_json_pretty, write_atomic};
use analyst_deck_protocol::config::{
    SnapshotFile, SnapshotLogRecord, SnapshotManifest, SnapshotReason, SnapshotRef,
};
use analyst_deck_protocol::ids::SnapshotId;
use anyhow::{anyhow, Context};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Pruning policy (PROTOCOL 4.4).
#[derive(Debug, Clone, Copy)]
pub struct Prune {
    pub keep_turns: usize,
    pub keep_days: i64,
}

impl Default for Prune {
    fn default() -> Self {
        Prune {
            keep_turns: 200,
            keep_days: 14,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotStore {
    project: PathBuf,
    dir: PathBuf,
}

impl SnapshotStore {
    pub fn new(project: &Path) -> SnapshotStore {
        SnapshotStore {
            project: project.to_path_buf(),
            dir: project.join(".present").join("history"),
        }
    }

    fn object_path(&self, sha: &str) -> PathBuf {
        self.dir.join("objects").join(&sha[..2]).join(sha)
    }

    fn manifest_path(&self, id: &SnapshotId) -> PathBuf {
        self.dir.join("snapshots").join(format!("{id}.json"))
    }

    fn put_object(&self, bytes: &[u8]) -> anyhow::Result<String> {
        let sha = sha256_hex(bytes);
        let path = self.object_path(&sha);
        if !path.is_file() {
            write_atomic(&path, &zstd::encode_all(bytes, 3)?)?;
        }
        Ok(sha)
    }

    fn get_object(&self, sha: &str) -> anyhow::Result<Vec<u8>> {
        let raw = std::fs::read(self.object_path(sha))
            .with_context(|| format!("snapshot object {sha} is missing"))?;
        let bytes = zstd::decode_all(raw.as_slice())?;
        if sha256_hex(&bytes) != sha {
            return Err(anyhow!("snapshot object {sha} is corrupt"));
        }
        Ok(bytes)
    }

    pub fn log(&self) -> anyhow::Result<Vec<SnapshotLogRecord>> {
        let path = self.dir.join("log.jsonl");
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for line in std::fs::read_to_string(path)?.lines() {
            if let Ok(rec) = serde_json::from_str::<SnapshotLogRecord>(line) {
                out.push(rec);
            }
        }
        Ok(out)
    }

    fn write_log(&self, records: &[SnapshotLogRecord]) -> anyhow::Result<()> {
        let mut text = String::new();
        for r in records {
            text.push_str(&serde_json::to_string(r)?);
            text.push('\n');
        }
        write_atomic(&self.dir.join("log.jsonl"), text.as_bytes())?;
        Ok(())
    }

    pub fn head(&self) -> anyhow::Result<Option<SnapshotId>> {
        Ok(self.log()?.last().map(|r| r.snapshot_id.clone()))
    }

    pub fn manifest(&self, id: &SnapshotId) -> anyhow::Result<SnapshotManifest> {
        let path = self.manifest_path(id);
        let bytes = std::fs::read(&path).with_context(|| format!("snapshot {id} not found"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Snapshot the deck as it is on disk now.
    pub fn take(
        &self,
        reason: SnapshotReason,
        turn_id: Option<&str>,
        label: Option<&str>,
        assets: Vec<String>,
        refs: Vec<SnapshotRef>,
    ) -> anyhow::Result<SnapshotId> {
        let store = DeckStore::new(&self.project);
        let deck = store.read_deck()?;
        let mut files = Vec::new();
        for rel in store.deck_files()? {
            let bytes =
                std::fs::read(self.project.join(&rel)).with_context(|| format!("reading {rel}"))?;
            files.push(SnapshotFile {
                path: rel,
                sha256: self.put_object(&bytes)?,
            });
        }
        let id = loop {
            let candidate = SnapshotId::new(new_id("h", 10)).map_err(|e| anyhow!("{e:?}"))?;
            if !self.manifest_path(&candidate).exists() {
                break candidate;
            }
        };
        let parent = self.head()?;
        let created = now_iso();
        let manifest = SnapshotManifest {
            snapshot_id: id.clone(),
            created: created.clone(),
            parent: parent.clone(),
            mode: deck.mode,
            files,
            assets,
            refs,
        };
        write_atomic(
            &self.manifest_path(&id),
            to_json_pretty(&manifest)?.as_bytes(),
        )?;
        let record = SnapshotLogRecord {
            ts: created,
            snapshot_id: id.clone(),
            parent,
            reason,
            turn_id: turn_id.map(str::to_string),
            label: label.map(str::to_string),
        };
        std::fs::create_dir_all(&self.dir)?;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("log.jsonl"))?;
        writeln!(log, "{}", serde_json::to_string(&record)?)?;
        Ok(id)
    }

    /// Write a snapshot's files back to `deck/`, removing deck files it does not contain, and
    /// record the result as a new `restore` snapshot. History is never rewritten.
    pub fn restore(&self, id: &SnapshotId) -> anyhow::Result<SnapshotId> {
        let manifest = self.manifest(id)?;
        let wanted: BTreeSet<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        // Fetch everything first so a missing object cannot leave a half-restored deck.
        let mut blobs = Vec::new();
        for f in &manifest.files {
            blobs.push((f.path.clone(), self.get_object(&f.sha256)?));
        }
        let deck_dir = self.project.join("deck");
        if deck_dir.is_dir() {
            for entry in walkdir::WalkDir::new(&deck_dir)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = crate::util::rel_slash(&self.project, entry.path());
                let managed = rel.ends_with(".json") || rel.ends_with(".py");
                if managed && !wanted.contains(rel.as_str()) {
                    crate::util::remove_file_patiently(entry.path())?;
                }
            }
        }
        for (rel, bytes) in blobs {
            write_atomic(&self.project.join(&rel), &bytes)?;
        }
        self.take(
            SnapshotReason::Restore,
            None,
            Some(&format!("restore of {id}")),
            manifest.assets,
            manifest.refs,
        )
    }

    /// Undo a turn: restore the snapshot taken just before it.
    pub fn revert_turn(&self, turn_id: &str) -> anyhow::Result<SnapshotId> {
        let log = self.log()?;
        let record = log
            .iter()
            .find(|r| r.reason == SnapshotReason::Turn && r.turn_id.as_deref() == Some(turn_id))
            .ok_or_else(|| anyhow!("no snapshot was taken for turn {turn_id}"))?;
        self.restore(&record.snapshot_id.clone())
    }

    /// Drop old `turn`, `restore`, and `manual` snapshots; pinned ones are always kept.
    pub fn prune(
        &self,
        policy: Prune,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<usize> {
        let log = self.log()?;
        let cutoff = now - chrono::Duration::days(policy.keep_days);
        let prunable: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.reason.is_pinned())
            .map(|(i, _)| i)
            .collect();
        let keep_from = prunable.len().saturating_sub(policy.keep_turns);
        let mut drop = BTreeSet::new();
        for (rank, &i) in prunable.iter().enumerate() {
            let recent = chrono::DateTime::parse_from_rfc3339(&log[i].ts)
                .map(|t| t.with_timezone(&chrono::Utc) >= cutoff)
                .unwrap_or(true);
            let is_head = i + 1 == log.len();
            if rank < keep_from && !recent && !is_head {
                drop.insert(i);
            }
        }
        if drop.is_empty() {
            return Ok(0);
        }
        let kept: Vec<SnapshotLogRecord> = log
            .iter()
            .enumerate()
            .filter(|(i, _)| !drop.contains(i))
            .map(|(_, r)| r.clone())
            .collect();
        let mut live = BTreeSet::new();
        for r in &kept {
            for f in self.manifest(&r.snapshot_id)?.files {
                live.insert(f.sha256);
            }
        }
        for &i in &drop {
            let _ = std::fs::remove_file(self.manifest_path(&log[i].snapshot_id));
        }
        self.write_log(&kept)?;
        let objects = self.dir.join("objects");
        if objects.is_dir() {
            for entry in walkdir::WalkDir::new(&objects)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if entry.file_type().is_file()
                    && !live.contains(entry.file_name().to_string_lossy().as_ref())
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(drop.len())
    }
}
