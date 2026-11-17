// Ported from Personal Analyst `crates/core/src/session.rs` (transcript part: `append_jsonl`,
// `read_jsonl`, `append_event`, `read_transcript`, `conversations`) at commit f229e4d.
// Adapted: a store of its own (no session record), optional fsync, a read that tolerates a
// truncated last line, conversation summaries, and pinned facts next to each transcript.
//! JSONL transcript store: `<root>/conversations/<id>.jsonl`, one event per line.

use crate::event::Event;
use crate::pinned::PinnedFacts;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Title length in `ConversationSummary` (characters).
pub const TITLE_MAX_CHARS: usize = 80;

#[derive(Debug, Clone)]
pub struct TranscriptStore {
    root: PathBuf,
    fsync: bool,
}

/// One row of the conversation list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: String,
    /// First user message, on one line, truncated to `TITLE_MAX_CHARS`; empty when there is
    /// no user message yet.
    pub title: String,
    /// Number of user messages.
    pub turns: usize,
    /// `ts` of the first event; empty for an empty transcript.
    pub created: String,
    /// `ts` of the last event; empty for an empty transcript.
    pub updated: String,
}

/// Result of a tolerant read.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadReport {
    pub events: Vec<Event>,
    /// 1-based numbers of lines that were not valid JSON objects and were skipped: a last
    /// line cut short by a crash, or such a line that later appends followed.
    pub skipped_lines: Vec<usize>,
}

impl TranscriptStore {
    /// A store rooted at a session or project state directory. Nothing is created until the
    /// first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        TranscriptStore {
            root: root.into(),
            fsync: false,
        }
    }

    /// Flush every appended line to disk (`fsync`) before returning. Off by default.
    pub fn with_fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn conversations_dir(&self) -> PathBuf {
        self.root.join("conversations")
    }

    /// A fresh conversation id (`<UTC stamp>-<6 hex>`); ids sort by creation time.
    pub fn new_conversation_id(&self) -> String {
        crate::util::stamped_id()
    }

    /// `conversations/<id>.jsonl`. Fails on an id that is not a plain file stem.
    pub fn transcript_path(&self, conv_id: &str) -> Result<PathBuf> {
        check_id(conv_id)?;
        Ok(self.conversations_dir().join(format!("{conv_id}.jsonl")))
    }

    /// `conversations/<id>.pinned.json`.
    pub fn pinned_path(&self, conv_id: &str) -> Result<PathBuf> {
        check_id(conv_id)?;
        Ok(self
            .conversations_dir()
            .join(format!("{conv_id}.pinned.json")))
    }

    pub fn exists(&self, conv_id: &str) -> bool {
        self.transcript_path(conv_id)
            .map(|p| p.is_file())
            .unwrap_or(false)
    }

    /// Append one event as one line.
    pub fn append(&self, conv_id: &str, event: &Event) -> Result<()> {
        let path = self.transcript_path(conv_id)?;
        append_line(&path, event, self.fsync)
    }

    /// Append several events.
    pub fn append_all(&self, conv_id: &str, events: &[Event]) -> Result<()> {
        for e in events {
            self.append(conv_id, e)?;
        }
        Ok(())
    }

    /// Read a conversation's events. A missing file is an empty conversation; lines that are
    /// not valid JSON objects (a truncated last line) are skipped.
    pub fn read(&self, conv_id: &str) -> Result<Vec<Event>> {
        Ok(self.read_report(conv_id)?.events)
    }

    /// `read`, also reporting which lines were skipped.
    pub fn read_report(&self, conv_id: &str) -> Result<ReadReport> {
        let path = self.transcript_path(conv_id)?;
        read_events(&path)
    }

    /// Conversation ids with transcripts on disk, sorted.
    pub fn conversation_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let rd = match std::fs::read_dir(self.conversations_dir()) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("listing {}", self.conversations_dir().display()))
            }
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                if check_id(stem).is_ok() {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// All conversations with a summary each, sorted by id. A transcript that cannot be read
    /// is listed with an empty summary.
    pub fn list(&self) -> Result<Vec<ConversationSummary>> {
        Ok(self
            .conversation_ids()?
            .into_iter()
            .map(|id| {
                let events = self.read(&id).unwrap_or_default();
                summarize(&id, &events)
            })
            .collect())
    }

    /// Summary of one conversation.
    pub fn summary(&self, conv_id: &str) -> Result<ConversationSummary> {
        Ok(summarize(conv_id, &self.read(conv_id)?))
    }

    /// Remove a conversation's transcript and pinned facts. Missing files are not an error.
    pub fn delete(&self, conv_id: &str) -> Result<()> {
        for path in [self.transcript_path(conv_id)?, self.pinned_path(conv_id)?] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
            }
        }
        Ok(())
    }

    /// Pinned facts of a conversation (empty when none were saved).
    pub fn load_pinned(&self, conv_id: &str) -> Result<PinnedFacts> {
        PinnedFacts::load(&self.pinned_path(conv_id)?)
    }

    pub fn save_pinned(&self, conv_id: &str, pinned: &PinnedFacts) -> Result<()> {
        pinned.save(&self.pinned_path(conv_id)?)
    }
}

fn check_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid conversation id `{id}` (letters, digits, `-`, `_`, `.` only)"
        ))
    }
}

fn one_line_title(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TITLE_MAX_CHARS {
        return flat;
    }
    let mut t: String = flat.chars().take(TITLE_MAX_CHARS - 1).collect();
    t.push('…');
    t
}

fn summarize(id: &str, events: &[Event]) -> ConversationSummary {
    let mut title = String::new();
    let mut turns = 0;
    for e in events {
        if let Event::User { text, .. } = e {
            if turns == 0 {
                title = one_line_title(text);
            }
            turns += 1;
        }
    }
    ConversationSummary {
        id: id.to_string(),
        title,
        turns,
        created: events
            .first()
            .map(|e| e.ts().to_string())
            .unwrap_or_default(),
        updated: events
            .last()
            .map(|e| e.ts().to_string())
            .unwrap_or_default(),
    }
}

fn append_line<T: Serialize>(path: &Path, value: &T, fsync: bool) -> Result<()> {
    let line = serde_json::to_string(value)?;
    if line.contains('\n') {
        bail!("JSONL line contains a newline");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    // A previous writer may have died mid-line: start on a fresh line so that this event
    // is not glued to the fragment.
    let len = f.metadata()?.len();
    let mut buf = Vec::with_capacity(line.len() + 2);
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            buf.push(b'\n');
        }
    }
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    f.write_all(&buf)
        .with_context(|| format!("writing {}", path.display()))?;
    if fsync {
        f.sync_data()
            .with_context(|| format!("syncing {}", path.display()))?;
    }
    Ok(())
}

/// Append one value as one JSON line (no fsync). Creates the parent directory.
pub fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    append_line(path, value, false)
}

/// Read a JSONL file strictly: a missing file is empty, blank lines are skipped, a line that
/// does not parse is an error naming the line.
pub fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let Some(text) = read_text(path)? else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: T = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: bad JSONL line", path.display(), i + 1))?;
        out.push(v);
    }
    Ok(out)
}

fn read_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        // A crash can cut a line inside a multi-byte character.
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("opening {}", path.display())),
    }
}

fn read_events(path: &Path) -> Result<ReadReport> {
    let mut report = ReadReport {
        events: vec![],
        skipped_lines: vec![],
    };
    let Some(text) = read_text(path)? else {
        return Ok(report);
    };
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(Event::Other(v)) if !v.is_object() || v.get("type").is_none() => {
                report.skipped_lines.push(i + 1)
            }
            Ok(e) => report.events.push(e),
            Err(_) => report.skipped_lines.push(i + 1),
        }
    }
    if !report.skipped_lines.is_empty() {
        tracing::warn!(
            "{}: skipped unreadable transcript lines {:?}",
            path.display(),
            report.skipped_lines
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, TranscriptStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = TranscriptStore::new(dir.path());
        (dir, s)
    }

    #[test]
    fn append_and_read_one_line_per_event() {
        let (_d, s) = store();
        assert!(s.read("c1").unwrap().is_empty());
        assert!(!s.exists("c1"));
        let events = vec![
            Event::user("hi"),
            Event::assistant("line1\nline2"),
            Event::tool_call("t1", "deck.outline", json!({})),
            Event::tool_result("t1", true, json!({"slides": []}), false),
            Event::other("skill_loaded", json!({"skill": "storyline"})),
        ];
        s.append_all("c1", &events).unwrap();
        assert!(s.exists("c1"));
        let path = s.transcript_path("c1").unwrap();
        assert!(path.ends_with("conversations/c1.jsonl"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 5);
        assert!(text.ends_with('\n'));
        assert_eq!(s.read("c1").unwrap(), events);
    }

    #[test]
    fn fsync_store_writes_the_same() {
        let (_d, s) = store();
        let s = s.with_fsync(true);
        s.append("c1", &Event::user("hi")).unwrap();
        s.append("c1", &Event::assistant("yo")).unwrap();
        assert_eq!(s.read("c1").unwrap().len(), 2);
    }

    #[test]
    fn truncated_last_line_is_tolerated_and_appends_continue() {
        let (_d, s) = store();
        s.append("c1", &Event::user("first")).unwrap();
        s.append("c1", &Event::assistant("ok")).unwrap();
        let path = s.transcript_path("c1").unwrap();
        // Simulate a crash mid-write: half a line, cut inside a multi-byte character.
        let mut bytes = std::fs::read(&path).unwrap();
        let partial = "{\"ts\":\"t\",\"type\":\"assistant\",\"text\":\"caf\u{e9}".as_bytes();
        bytes.extend_from_slice(&partial[..partial.len() - 1]);
        std::fs::write(&path, &bytes).unwrap();

        let r = s.read_report("c1").unwrap();
        assert_eq!(r.events.len(), 2);
        assert_eq!(r.skipped_lines, vec![3]);

        // The next append starts on a fresh line.
        s.append("c1", &Event::user("second")).unwrap();
        let r = s.read_report("c1").unwrap();
        assert_eq!(r.events.len(), 3);
        assert_eq!(r.skipped_lines, vec![3]);
        assert!(matches!(&r.events[2], Event::User { text, .. } if text == "second"));
        assert_eq!(s.summary("c1").unwrap().turns, 2);
    }

    #[test]
    fn non_event_lines_are_skipped() {
        let (_d, s) = store();
        let path = s.transcript_path("c1").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "42\n\n{\"no_type\":1}\n{\"ts\":\"t\",\"type\":\"user\",\"text\":\"hi\"}\n",
        )
        .unwrap();
        let r = s.read_report("c1").unwrap();
        assert_eq!(r.events.len(), 1);
        assert_eq!(r.skipped_lines, vec![1, 3]);
    }

    #[test]
    fn list_conversations_with_summaries() {
        let (_d, s) = store();
        assert!(s.list().unwrap().is_empty());
        let long = format!("Build   a deck\nabout {}", "x".repeat(200));
        s.append(
            "b",
            &Event::Snapshot {
                ts: "2026-11-15T10:00:00Z".into(),
                snapshot_id: "s".into(),
                reason: "turn".into(),
            },
        )
        .unwrap();
        s.append(
            "b",
            &Event::User {
                ts: "2026-11-15T10:00:01Z".into(),
                text: long,
                attachments: None,
            },
        )
        .unwrap();
        s.append(
            "b",
            &Event::User {
                ts: "2026-11-15T10:05:00Z".into(),
                text: "and a chart".into(),
                attachments: None,
            },
        )
        .unwrap();
        s.append("a", &Event::assistant("no user yet")).unwrap();
        s.save_pinned("a", &PinnedFacts::default()).unwrap();
        // Unrelated files are not conversations.
        std::fs::write(s.conversations_dir().join("notes.txt"), "x").unwrap();

        let list = s.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "a");
        assert_eq!(list[0].title, "");
        assert_eq!(list[0].turns, 0);
        let b = &list[1];
        assert_eq!(b.id, "b");
        assert_eq!(b.turns, 2);
        assert_eq!(b.created, "2026-11-15T10:00:00Z");
        assert_eq!(b.updated, "2026-11-15T10:05:00Z");
        assert!(b.title.starts_with("Build a deck about xxx"));
        assert_eq!(b.title.chars().count(), TITLE_MAX_CHARS);
        assert!(b.title.ends_with('…'));
        assert_eq!(s.conversation_ids().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn ids_cannot_escape_the_store() {
        let (_d, s) = store();
        for bad in ["", "../x", "a/b", "a\\b", ".hidden", "a b"] {
            assert!(s.append(bad, &Event::user("x")).is_err(), "{bad}");
            assert!(s.read(bad).is_err(), "{bad}");
        }
        let id = s.new_conversation_id();
        s.append(&id, &Event::user("x")).unwrap();
        assert_eq!(s.conversation_ids().unwrap(), vec![id]);
    }

    #[test]
    fn delete_removes_transcript_and_pinned() {
        let (_d, s) = store();
        s.append("c1", &Event::user("x")).unwrap();
        let mut p = PinnedFacts::default();
        p.add("fact");
        s.save_pinned("c1", &p).unwrap();
        assert_eq!(s.load_pinned("c1").unwrap(), p);
        s.delete("c1").unwrap();
        assert!(!s.exists("c1"));
        assert!(s.load_pinned("c1").unwrap().is_empty());
        s.delete("c1").unwrap();
    }

    #[test]
    fn generic_jsonl_helpers_are_strict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/runs.jsonl");
        assert!(read_jsonl::<serde_json::Value>(&path).unwrap().is_empty());
        append_jsonl(&path, &json!({"a": 1})).unwrap();
        append_jsonl(&path, &json!({"a": 2})).unwrap();
        assert_eq!(read_jsonl::<serde_json::Value>(&path).unwrap().len(), 2);
        std::fs::write(&path, "{\"a\":1}\n{\"a\":").unwrap();
        let err = read_jsonl::<serde_json::Value>(&path).unwrap_err();
        assert!(err.to_string().contains(":2: bad JSONL line"));
    }
}
