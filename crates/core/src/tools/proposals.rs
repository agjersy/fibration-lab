//! Edit proposals: `edits` (old/new, exact-once matching) or full `content`, create, delete,
//! with conflict detection by content hash of the file the proposal was based on.
//!
//! Paths are confined twice: lexically (`normalize_rel_path`, reserved directories) and
//! physically (`resolve_in_repo`, no writes through symlinks). Line endings of the existing
//! file are preserved.

use crate::util::{is_reserved_path, is_symlink, normalize_rel_path, resolve_in_repo, sha256_hex};
use analyst_protocol::transcript::ProposalKind;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub id: String,
    pub kind: ProposalKind,
    pub path: String,
    /// Full new content (None for delete).
    pub new_content: Option<String>,
    /// SHA-256 of the file the proposal was based on (None for create).
    pub base_hash: Option<String>,
    pub summary: String,
    pub diff: String,
}

/// Lexical and physical confinement shared by build and apply. Returns the absolute path.
fn confine(repo: &Path, rel: &str) -> Result<PathBuf> {
    if is_reserved_path(rel) {
        return Err(anyhow!(
            "`{rel}` is under .git/ or .analyst/, which are never editable"
        ));
    }
    let lexical = repo.join(rel);
    if is_symlink(&lexical) {
        return Err(anyhow!(
            "`{rel}` is a symbolic link; edit the file it points to instead"
        ));
    }
    resolve_in_repo(repo, rel, false).map_err(|e| anyhow!(e))
}

fn read_existing(repo: &Path, rel: &str) -> Result<String> {
    let full = confine(repo, rel)?;
    std::fs::read_to_string(&full).map_err(|e| anyhow!("cannot read `{rel}`: {e}"))
}

/// Whether a text uses CRLF line endings (every newline is preceded by a carriage return).
pub fn is_crlf(text: &str) -> bool {
    let crlf = text.matches("\r\n").count();
    crlf > 0 && crlf == text.matches('\n').count()
}

fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

/// Build a proposal for `fs.propose_edit`, `fs.propose_create`, or `fs.propose_delete`.
pub fn build(repo: &Path, id: &str, tool: &str, args: &Value) -> Result<Proposal> {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`path` is required"))?;
    let path = normalize_rel_path(raw).map_err(|e| anyhow!(e))?;
    match tool {
        "fs.propose_create" => {
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`content` is required"))?;
            let full = confine(repo, &path)?;
            if std::fs::symlink_metadata(&full).is_ok() {
                return Err(anyhow!("`{path}` already exists; use fs.propose_edit"));
            }
            Ok(Proposal {
                id: id.to_string(),
                kind: ProposalKind::Create,
                path: path.clone(),
                new_content: Some(content.to_string()),
                base_hash: None,
                summary: format!("create {path} ({} lines)", content.lines().count()),
                diff: unified_diff("", content, &path),
            })
        }
        "fs.propose_delete" => {
            let existing = read_existing(repo, &path)?;
            Ok(Proposal {
                id: id.to_string(),
                kind: ProposalKind::Delete,
                path: path.clone(),
                new_content: None,
                base_hash: Some(sha256_hex(existing.as_bytes())),
                summary: format!("delete {path}"),
                diff: unified_diff(&existing, "", &path),
            })
        }
        "fs.propose_edit" => {
            let existing = read_existing(repo, &path)?;
            let new_content = if let Some(edits) = args.get("edits").and_then(Value::as_array) {
                apply_edits(&existing, edits)?
            } else if let Some(c) = args.get("content").and_then(Value::as_str) {
                // Models emit LF; keep a CRLF file CRLF.
                if is_crlf(&existing) {
                    to_crlf(c)
                } else {
                    c.to_string()
                }
            } else {
                return Err(anyhow!("fs.propose_edit needs `edits` or `content`"));
            };
            if new_content == existing {
                return Err(anyhow!("edit produces no change to `{path}`"));
            }
            let diff = unified_diff(&existing, &new_content, &path);
            let (added, removed) = count_changes(&diff);
            Ok(Proposal {
                id: id.to_string(),
                kind: ProposalKind::Edit,
                path: path.clone(),
                new_content: Some(new_content),
                base_hash: Some(sha256_hex(existing.as_bytes())),
                summary: format!("edit {path} (+{added} -{removed})"),
                diff,
            })
        }
        other => Err(anyhow!("`{other}` is not an edit tool")),
    }
}

/// Apply `{old, new}` edits; each `old` must occur exactly once in the current text. A CRLF
/// file is matched and rewritten line-ending-insensitively: `old`/`new` may use LF, and the
/// result keeps CRLF.
pub fn apply_edits(existing: &str, edits: &[Value]) -> Result<String> {
    if is_crlf(existing) {
        let normalised: Vec<Value> = edits
            .iter()
            .map(|e| {
                let mut e = e.clone();
                for key in ["old", "new"] {
                    if let Some(t) = e.get(key).and_then(Value::as_str) {
                        e[key] = Value::String(to_lf(t));
                    }
                }
                e
            })
            .collect();
        return Ok(to_crlf(&apply_edits_exact(&to_lf(existing), &normalised)?));
    }
    apply_edits_exact(existing, edits)
}

fn apply_edits_exact(existing: &str, edits: &[Value]) -> Result<String> {
    let mut text = existing.to_string();
    for (i, e) in edits.iter().enumerate() {
        let old = e
            .get("old")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("edit {i}: `old` is required"))?;
        let new = e
            .get("new")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("edit {i}: `new` is required"))?;
        if old.is_empty() {
            return Err(anyhow!("edit {i}: `old` must not be empty"));
        }
        let count = text.matches(old).count();
        if count == 0 {
            return Err(anyhow!(
                "edit {i}: `old` text not found in the file (re-read the file; it may have changed)"
            ));
        }
        if count > 1 {
            return Err(anyhow!(
                "edit {i}: `old` text occurs {count} times; include more context so it matches exactly once"
            ));
        }
        text = text.replacen(old, new, 1);
    }
    Ok(text)
}

/// Apply a proposal to the working tree, rejecting it when the file changed since it was
/// built (conflict). Creation uses create-new semantics, so a file that appeared while the
/// proposal was pending is never overwritten. Nothing is written through a symlink or into a
/// reserved directory.
pub fn apply(repo: &Path, p: &Proposal) -> Result<()> {
    use std::io::Write;
    let full = confine(repo, &p.path)?;
    if let Some(h) = &p.base_hash {
        let current = std::fs::read(&full).map_err(|e| anyhow!("cannot read `{}`: {e}", p.path))?;
        if sha256_hex(&current) != *h {
            return Err(anyhow!(
                "conflict: `{}` changed since the proposal was made; re-read it and propose again",
                p.path
            ));
        }
    }
    let content = p.new_content.as_deref().unwrap_or("");
    match p.kind {
        ProposalKind::Delete => {
            std::fs::remove_file(&full).map_err(|e| anyhow!("deleting `{}`: {e}", p.path))?;
        }
        ProposalKind::Create => {
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow!("creating the directory for `{}`: {e}", p.path))?;
            }
            // The parent may have been replaced by a link while the proposal was pending.
            let full = confine(repo, &p.path)?;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&full)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        anyhow!(
                            "conflict: `{}` was created by someone else while this proposal was pending; read it and use fs.propose_edit instead",
                            p.path
                        )
                    } else {
                        anyhow!("creating `{}`: {e}", p.path)
                    }
                })?;
            f.write_all(content.as_bytes())
                .map_err(|e| anyhow!("writing `{}`: {e}", p.path))?;
        }
        _ => {
            std::fs::write(&full, content).map_err(|e| anyhow!("writing `{}`: {e}", p.path))?;
        }
    }
    Ok(())
}

fn count_changes(diff: &str) -> (usize, usize) {
    let mut a = 0;
    let mut r = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            a += 1;
        } else if l.starts_with('-') {
            r += 1;
        }
    }
    (a, r)
}

/// A minimal unified diff (LCS on lines, 3 lines of context).
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = diff_ops(&a, &b);
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    // Group ops into hunks with context.
    let ctx = 3usize;
    let mut i = 0;
    while i < ops.len() {
        if ops[i].0 == ' ' {
            i += 1;
            continue;
        }
        let start = i.saturating_sub(ctx);
        let mut end = i;
        let mut last_change = i;
        while end < ops.len() && end <= last_change + ctx {
            if ops[end].0 != ' ' {
                last_change = end;
            }
            end += 1;
        }
        let end = end.min(ops.len());
        let (mut a_start, mut b_start, mut a_len, mut b_len) = (0usize, 0usize, 0usize, 0usize);
        let mut body = String::new();
        for (k, (tag, ai, bi)) in ops.iter().enumerate() {
            if k < start {
                continue;
            }
            if k >= end {
                break;
            }
            if a_len == 0 && b_len == 0 {
                a_start = ai.map(|x| x + 1).unwrap_or(0);
                b_start = bi.map(|x| x + 1).unwrap_or(0);
            }
            match tag {
                ' ' => {
                    a_len += 1;
                    b_len += 1;
                    body.push_str(&format!(" {}\n", a[ai.unwrap_or(0)]));
                }
                '-' => {
                    a_len += 1;
                    body.push_str(&format!("-{}\n", a[ai.unwrap_or(0)]));
                }
                _ => {
                    b_len += 1;
                    body.push_str(&format!("+{}\n", b[bi.unwrap_or(0)]));
                }
            }
        }
        if a_start == 0 && a_len > 0 {
            a_start = 1;
        }
        if b_start == 0 && b_len > 0 {
            b_start = 1;
        }
        out.push_str(&format!("@@ -{a_start},{a_len} +{b_start},{b_len} @@\n"));
        out.push_str(&body);
        i = end;
    }
    out
}

/// Line-level diff ops: (' ' | '-' | '+', index in a, index in b).
fn diff_ops<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(char, Option<usize>, Option<usize>)> {
    let n = a.len();
    let m = b.len();
    // Guard against quadratic blowup on huge files: fall back to whole replacement.
    if n * m > 4_000_000 {
        let mut ops = Vec::new();
        for i in 0..n {
            ops.push(('-', Some(i), None));
        }
        for j in 0..m {
            ops.push(('+', None, Some(j)));
        }
        return ops;
    }
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push((' ', Some(i), Some(j)));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(('-', Some(i), None));
            i += 1;
        } else {
            ops.push(('+', None, Some(j)));
            j += 1;
        }
    }
    while i < n {
        ops.push(('-', Some(i), None));
        i += 1;
    }
    while j < m {
        ops.push(('+', None, Some(j)));
        j += 1;
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edits_require_exact_once_match() {
        let text = "a\nb\na\n";
        assert!(apply_edits(text, &[json!({"old":"a","new":"c"})]).is_err());
        assert!(apply_edits(text, &[json!({"old":"zz","new":"c"})]).is_err());
        assert_eq!(
            apply_edits(text, &[json!({"old":"b","new":"c"})]).unwrap(),
            "a\nc\na\n"
        );
    }

    #[test]
    fn build_apply_and_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(repo.join("f.py"), "x = 1\ny = 2\n").unwrap();
        let p = build(
            repo,
            "p1",
            "fs.propose_edit",
            &json!({"path":"f.py","edits":[{"old":"y = 2","new":"y = 3"}]}),
        )
        .unwrap();
        assert_eq!(p.kind, ProposalKind::Edit);
        assert!(p.diff.contains("-y = 2"));
        assert!(p.diff.contains("+y = 3"));
        assert_eq!(p.summary, "edit f.py (+1 -1)");
        // Concurrent change -> conflict.
        std::fs::write(repo.join("f.py"), "x = 9\ny = 2\n").unwrap();
        let err = apply(repo, &p).unwrap_err().to_string();
        assert!(err.contains("conflict"));
        std::fs::write(repo.join("f.py"), "x = 1\ny = 2\n").unwrap();
        apply(repo, &p).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("f.py")).unwrap(),
            "x = 1\ny = 3\n"
        );

        let c = build(
            repo,
            "p2",
            "fs.propose_create",
            &json!({"path":"new/g.py","content":"g\n"}),
        )
        .unwrap();
        apply(repo, &c).unwrap();
        assert!(repo.join("new/g.py").is_file());
        assert!(build(
            repo,
            "p3",
            "fs.propose_create",
            &json!({"path":"new/g.py","content":"g\n"})
        )
        .is_err());
        let d = build(repo, "p4", "fs.propose_delete", &json!({"path":"new/g.py"})).unwrap();
        apply(repo, &d).unwrap();
        assert!(!repo.join("new/g.py").exists());
        assert!(build(
            repo,
            "p5",
            "fs.propose_edit",
            &json!({"path":"../x","content":""})
        )
        .is_err());
    }

    /// A9: CRLF files can be edited with LF `old`/`new`, and stay CRLF.
    #[test]
    fn crlf_files_are_editable_and_stay_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(repo.join("w.py"), "x = 1\r\ny = 2\r\nz = 3\r\n").unwrap();
        let p = build(
            repo,
            "p1",
            "fs.propose_edit",
            &json!({"path":"w.py","edits":[{"old":"x = 1\ny = 2","new":"x = 1\ny = 20\nq = 0"}]}),
        )
        .unwrap();
        apply(repo, &p).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("w.py")).unwrap(),
            "x = 1\r\ny = 20\r\nq = 0\r\nz = 3\r\n"
        );
        assert_eq!(p.summary, "edit w.py (+2 -1)");
        let full = build(
            repo,
            "p2",
            "fs.propose_edit",
            &json!({"path":"w.py","content":"a = 1\nb = 2\n"}),
        )
        .unwrap();
        assert_eq!(full.new_content.as_deref(), Some("a = 1\r\nb = 2\r\n"));
        // LF files are untouched by the CRLF path.
        assert_eq!(
            apply_edits("a\nb\n", &[json!({"old":"a\nb","new":"c"})]).unwrap(),
            "c\n"
        );
    }

    /// A9: a file the user creates while a create-proposal is pending is not overwritten.
    #[test]
    fn create_does_not_overwrite_a_file_that_appeared() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let p = build(
            repo,
            "p1",
            "fs.propose_create",
            &json!({"path":"notes/n.md","content":"agent\n"}),
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("notes")).unwrap();
        std::fs::write(repo.join("notes/n.md"), "user\n").unwrap();
        let err = apply(repo, &p).unwrap_err().to_string();
        assert!(err.contains("conflict") && err.contains("created"), "{err}");
        assert_eq!(
            std::fs::read_to_string(repo.join("notes/n.md")).unwrap(),
            "user\n"
        );
    }

    /// G4 and G7: reserved directories and symlinks are refused at build and at apply.
    #[cfg(unix)]
    #[test]
    fn reserved_and_symlinked_paths_are_refused() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
        std::fs::write(repo.join(".git/config"), "[core]\n").unwrap();
        std::fs::write(outside.path().join("victim.txt"), "v\n").unwrap();
        symlink(outside.path(), repo.join("esc")).unwrap();
        symlink(outside.path().join("victim.txt"), repo.join("alias.txt")).unwrap();
        for (tool, args) in [
            (
                "fs.propose_edit",
                json!({"path":".git/config","content":"x"}),
            ),
            (
                "fs.propose_create",
                json!({"path":".git/hooks/pre-commit","content":"x"}),
            ),
            (
                "fs.propose_create",
                json!({"path":".GIT/hooks/pre-commit","content":"x"}),
            ),
            (
                "fs.propose_create",
                json!({"path":".analyst/project.toml","content":"x"}),
            ),
            (
                "fs.propose_create",
                json!({"path":"esc/new.txt","content":"x"}),
            ),
            (
                "fs.propose_edit",
                json!({"path":"esc/victim.txt","content":"x"}),
            ),
            ("fs.propose_edit", json!({"path":"alias.txt","content":"x"})),
            ("fs.propose_delete", json!({"path":"alias.txt"})),
            ("fs.propose_delete", json!({"path":"esc/victim.txt"})),
        ] {
            assert!(build(repo, "p", tool, &args).is_err(), "{tool} {args}");
        }
        // A proposal built before the directory became a link is refused at apply.
        let p = build(
            repo,
            "p",
            "fs.propose_create",
            &json!({"path":"later/f.txt","content":"x"}),
        )
        .unwrap();
        symlink(outside.path(), repo.join("later")).unwrap();
        assert!(apply(repo, &p).is_err());
        assert!(!outside.path().join("f.txt").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("victim.txt")).unwrap(),
            "v\n"
        );
        // Hand-built proposals (the service applies user-edited ones) are confined too.
        let forged = Proposal {
            id: "f".into(),
            kind: ProposalKind::Create,
            path: ".git/hooks/post-commit".into(),
            new_content: Some("x".into()),
            base_hash: None,
            summary: String::new(),
            diff: String::new(),
        };
        assert!(apply(repo, &forged).is_err());
    }

    #[test]
    fn unified_diff_shape() {
        let d = unified_diff("a\nb\nc\nd\ne\nf\ng\n", "a\nb\nc\nX\ne\nf\ng\n", "p");
        assert!(d.starts_with("--- a/p\n+++ b/p\n@@ -1,7 +1,7 @@\n"));
        assert!(d.contains("-d\n+X\n"));
    }
}
