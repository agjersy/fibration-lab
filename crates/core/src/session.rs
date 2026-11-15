//! Session store under `$ANALYST_HOME/sessions/<id>/` (PROTOCOL 2.3): `session.toml`,
//! transcript JSONL per conversation, `runs.jsonl`, `artifacts/`, and the "files changed
//! since last turn" tree diff.

use crate::checkpoint;
use analyst_protocol::kernel::RunMeta;
use analyst_protocol::session::{SessionRecord, SessionRun};
use analyst_protocol::transcript::Event;
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Session {
    pub home: PathBuf,
    pub dir: PathBuf,
    pub record: SessionRecord,
}

pub fn sessions_dir(home: &Path) -> PathBuf {
    home.join("sessions")
}

impl Session {
    /// Create a new session for a repo. `model` is `"<provider>/<id>"`.
    pub fn create(home: &Path, repo: &Path, target: &str, model: &str) -> Result<Session> {
        let id = crate::util::stamped_id();
        let dir = sessions_dir(home).join(&id);
        std::fs::create_dir_all(dir.join("conversations"))
            .with_context(|| format!("creating {}", dir.display()))?;
        std::fs::create_dir_all(dir.join("artifacts"))?;
        let repo_abs = std::fs::canonicalize(repo)
            .with_context(|| format!("resolving repo {}", repo.display()))?;
        let record = SessionRecord {
            id: id.clone(),
            repo: repo_abs.to_string_lossy().to_string(),
            target: target.to_string(),
            model: model.to_string(),
            created_at: crate::util::now_rfc3339(),
            last_tree_hash: None,
        };
        let s = Session {
            home: home.to_path_buf(),
            dir,
            record,
        };
        s.save()?;
        Ok(s)
    }

    /// Open an existing session by id.
    pub fn open(home: &Path, id: &str) -> Result<Session> {
        let dir = sessions_dir(home).join(id);
        let text = std::fs::read_to_string(dir.join("session.toml"))
            .with_context(|| format!("no session `{id}` under {}", sessions_dir(home).display()))?;
        let record: SessionRecord = toml::from_str(&text).context("parsing session.toml")?;
        Ok(Session {
            home: home.to_path_buf(),
            dir,
            record,
        })
    }

    /// Find the most recent session for a repo and target, if any.
    pub fn find_latest(home: &Path, repo: &Path, target: &str) -> Result<Option<Session>> {
        let repo_abs = std::fs::canonicalize(repo)?.to_string_lossy().to_string();
        let mut found: Vec<Session> = list(home)?
            .into_iter()
            .filter(|s| s.record.repo == repo_abs && s.record.target == target)
            .collect();
        found.sort_by(|a, b| a.record.id.cmp(&b.record.id));
        Ok(found.pop())
    }

    pub fn save(&self) -> Result<()> {
        let text = toml::to_string_pretty(&self.record)?;
        std::fs::write(self.dir.join("session.toml"), text)
            .with_context(|| format!("writing session.toml in {}", self.dir.display()))
    }

    pub fn id(&self) -> &str {
        &self.record.id
    }

    pub fn repo(&self) -> PathBuf {
        PathBuf::from(&self.record.repo)
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.dir.join("artifacts")
    }

    pub fn conversations_dir(&self) -> PathBuf {
        self.dir.join("conversations")
    }

    pub fn transcript_path(&self, conv_id: &str) -> PathBuf {
        self.conversations_dir().join(format!("{conv_id}.jsonl"))
    }

    /// Append one transcript event.
    pub fn append_event(&self, conv_id: &str, event: &Event) -> Result<()> {
        append_jsonl(&self.transcript_path(conv_id), event)
    }

    /// Read a conversation's events.
    pub fn read_transcript(&self, conv_id: &str) -> Result<Vec<Event>> {
        read_jsonl(&self.transcript_path(conv_id))
    }

    /// Conversation ids with transcripts on disk.
    pub fn conversations(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.conversations_dir()) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(stem) = name.strip_suffix(".jsonl") {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Mirror a RunMeta into `runs.jsonl`.
    pub fn append_run(&self, meta: &RunMeta) -> Result<()> {
        let run = SessionRun {
            meta: meta.clone(),
            target: self.record.target.clone(),
        };
        append_jsonl(&self.dir.join("runs.jsonl"), &run)
    }

    pub fn read_runs(&self) -> Result<Vec<SessionRun>> {
        read_jsonl(&self.dir.join("runs.jsonl"))
    }

    /// Latest record for a run id (runs are appended again when they finish).
    pub fn find_run(&self, run_id: &str) -> Result<Option<SessionRun>> {
        Ok(self
            .read_runs()?
            .into_iter()
            .rfind(|r| r.meta.run_id == run_id))
    }

    /// Store bytes under `artifacts/<name>`; returns the absolute path.
    pub fn spill_artifact(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.artifacts_dir().join(name);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /// Local path for a pulled artifact, keyed `<run_id>/<relative path>`.
    pub fn pulled_artifact_path(&self, run_id: &str, rel: &str) -> PathBuf {
        self.artifacts_dir().join(run_id).join(rel)
    }

    pub fn set_last_tree_hash(&mut self, hash: Option<String>) -> Result<()> {
        self.record.last_tree_hash = hash;
        self.save()
    }

    fn worktree_marker(&self) -> PathBuf {
        self.dir.join("last_worktree_tree")
    }

    /// Files changed in the working tree since the last call (tree diff against the recorded
    /// worktree tree). The first call records a baseline and returns an empty list.
    pub fn files_changed_since_last_turn(&self) -> Result<Vec<String>> {
        let repo = self.repo();
        let now = checkpoint::worktree_tree(&repo)?;
        let marker = self.worktree_marker();
        let prev = std::fs::read_to_string(&marker)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        std::fs::write(&marker, &now)?;
        match prev {
            Some(p) if p != now => checkpoint::changed_files(&repo, &p, &now),
            _ => Ok(vec![]),
        }
    }

    /// Record the current worktree as the baseline (after the agent's own edits).
    pub fn mark_worktree(&self) -> Result<()> {
        let now = checkpoint::worktree_tree(&self.repo())?;
        std::fs::write(self.worktree_marker(), now)?;
        Ok(())
    }
}

/// All sessions under a home.
pub fn list(home: &Path) -> Result<Vec<Session>> {
    let mut out = Vec::new();
    let dir = sessions_dir(home);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            if let Ok(s) = Session::open(home, &id) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| a.record.id.cmp(&b.record.id));
    Ok(out)
}

/// One lock per JSONL file, shared by every writer in the process.
fn file_lock(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(path.to_path_buf()).or_default().clone()
}

/// Append one JSON line. The line and its newline go out in a single `write` on an
/// `O_APPEND` handle while holding the file's process-wide lock, so concurrent writers never
/// interleave. A previous torn line (no trailing newline) is closed off first.
pub fn append_jsonl<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut line = serde_json::to_string(value)?;
    if line.contains('\n') {
        return Err(anyhow!("JSONL line contains a newline"));
    }
    line.push('\n');
    let lock = file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > 0 {
        let mut last = [0u8; 1];
        if f.seek(SeekFrom::End(-1)).is_ok() && f.read_exact(&mut last).is_ok() && last[0] != b'\n'
        {
            line.insert(0, '\n');
        }
    }
    f.write_all(line.as_bytes())
        .with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// Read a JSONL file. Lines that do not parse (torn writes, records from a newer version) are
/// skipped with a warning instead of making the whole file unreadable.
pub fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    let mut out = Vec::new();
    let mut reader = std::io::BufReader::new(f);
    let mut buf = Vec::new();
    let mut n = 0usize;
    loop {
        buf.clear();
        let read = reader
            .read_until(b'\n', &mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        n += 1;
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(line) {
            Ok(v) => out.push(v),
            Err(e) => tracing::warn!("{}:{n}: skipping bad JSONL line: {e}", path.display()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use analyst_protocol::kernel::RunStatus;

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        checkpoint::git(dir.path(), &["init", "-q"], None).unwrap();
        std::fs::write(dir.path().join("a.py"), "a = 1\n").unwrap();
        checkpoint::git(dir.path(), &["add", "-A"], None).unwrap();
        checkpoint::git(dir.path(), &["commit", "-q", "-m", "init"], None).unwrap();
        dir
    }

    #[test]
    fn create_open_transcript_runs_and_changes() {
        let home = tempfile::tempdir().unwrap();
        let repo = temp_repo();
        let s = Session::create(home.path(), repo.path(), "local", "p/m").unwrap();
        let again = Session::open(home.path(), s.id()).unwrap();
        assert_eq!(again.record, s.record);
        assert_eq!(list(home.path()).unwrap().len(), 1);
        assert_eq!(
            Session::find_latest(home.path(), repo.path(), "local")
                .unwrap()
                .unwrap()
                .id(),
            s.id()
        );

        s.append_event(
            "c1",
            &Event::User {
                ts: "t".into(),
                text: "hi".into(),
                attachments: None,
            },
        )
        .unwrap();
        s.append_event(
            "c1",
            &Event::Assistant {
                ts: "t".into(),
                text: "line1\nline2".into(),
            },
        )
        .unwrap();
        let events = s.read_transcript("c1").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(s.conversations().unwrap(), vec!["c1"]);

        let meta = RunMeta {
            run_id: "r1".into(),
            script: "scripts/a.py".into(),
            args: vec![],
            status: RunStatus::Running,
            exit_code: None,
            started_at: "t".into(),
            finished_at: None,
            out_dir: "out/a/r1".into(),
            sandbox: "none".into(),
            tree_hash: None,
            pid: None,
            host: "h".into(),
        };
        s.append_run(&meta).unwrap();
        let mut done = meta.clone();
        done.status = RunStatus::Done;
        s.append_run(&done).unwrap();
        assert_eq!(s.read_runs().unwrap().len(), 2);
        assert_eq!(
            s.find_run("r1").unwrap().unwrap().meta.status,
            RunStatus::Done
        );
        assert_eq!(s.read_runs().unwrap()[0].target, "local");

        let p = s.spill_artifact("c1/result.json", b"{}").unwrap();
        assert!(p.starts_with(s.artifacts_dir()));

        assert!(
            s.files_changed_since_last_turn().unwrap().is_empty(),
            "baseline"
        );
        std::fs::write(repo.path().join("b.py"), "b\n").unwrap();
        assert_eq!(s.files_changed_since_last_turn().unwrap(), vec!["b.py"]);
        assert!(s.files_changed_since_last_turn().unwrap().is_empty());
    }

    /// A7: a torn line must not make the file unreadable, and the next append must not be
    /// glued onto it.
    #[test]
    fn jsonl_reader_skips_torn_lines_and_appends_recover() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.jsonl");
        append_jsonl(&p, &serde_json::json!({"n": 1})).unwrap();
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(b"{\"n\": 2, \"torn").unwrap();
        }
        append_jsonl(&p, &serde_json::json!({"n": 3})).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"\xff\xfe not utf8\n")
            .unwrap();
        append_jsonl(&p, &serde_json::json!({"n": 4})).unwrap();
        let v: Vec<serde_json::Value> = read_jsonl(&p).unwrap();
        let ns: Vec<i64> = v.iter().map(|x| x["n"].as_i64().unwrap()).collect();
        assert_eq!(ns, vec![1, 3, 4]);
    }

    #[test]
    fn concurrent_appends_never_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("runs.jsonl");
        let payload = "x".repeat(20_000);
        let mut handles = Vec::new();
        for t in 0..8 {
            let p = p.clone();
            let payload = payload.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    append_jsonl(&p, &serde_json::json!({"t": t, "i": i, "pad": payload})).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().count(), 200);
        for l in text.lines() {
            serde_json::from_str::<serde_json::Value>(l).unwrap();
        }
    }
}
