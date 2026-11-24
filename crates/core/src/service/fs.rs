//! The workspace-files service ([`WorkspaceFiles`]): repo filesystem operations behind the
//! UI's `fs_*` commands and the agent's file tools: tree (respecting `.gitignore`), read and
//! write with SHA-256 hashes and conflict detection, create, delete (OS trash when available),
//! rename, and search through `rg --json`.

use super::error::{ServiceError, ServiceResult};
use crate::util::{is_reserved_path, normalize_rel_path, resolve_in_repo, sha256_hex};
use base64::Engine;
use fibration_protocol::ui::{FileContent, FileEntry, FileKind, SearchHit, WriteResult};
use std::path::{Path, PathBuf};

/// Directories never shown or searched.
const ALWAYS_HIDDEN: &[&str] = &[
    ".git",
    ".venv",
    "__pycache__",
    ".ruff_cache",
    "node_modules",
];

/// `fs_read` refuses text files larger than this.
pub const MAX_TEXT_BYTES: u64 = 5 * 1024 * 1024;
/// `fs_read_binary` and `figure_data` refuse files larger than this.
pub const MAX_BINARY_BYTES: u64 = 50 * 1024 * 1024;

fn rel(raw: &str) -> ServiceResult<String> {
    normalize_rel_path(raw).map_err(ServiceError::invalid)
}

/// Resolve a repo-relative path (empty or `.` means the root) for reading. The result provably
/// stays inside the repository: symlinks are followed and must not lead out of it (G7).
pub fn resolve(repo: &Path, raw: &str) -> ServiceResult<(String, PathBuf)> {
    let raw = raw.trim();
    let r = if raw.is_empty() || raw == "." || raw == "./" {
        String::new()
    } else {
        rel(raw)?
    };
    let abs = resolve_in_repo(repo, &r, false).map_err(ServiceError::invalid)?;
    Ok((r, abs))
}

fn reserved(rel: &str) -> ServiceError {
    ServiceError::policy_denied(format!(
        "`{rel}` is under a reserved directory (.git/ or .fibration/) and cannot be changed from the app"
    ))
}

/// Resolve a repo-relative path for a mutation (write, create, delete, rename). Paths under
/// `.git/` and `.fibration/` are refused (G4), lexically and after symlink resolution. With
/// `follow_last` the final component may be a symlink to a place inside the repo (writes go
/// through it); without, the final component itself is addressed (delete and rename act on
/// the link, never on its target).
pub fn resolve_mut(repo: &Path, raw: &str, follow_last: bool) -> ServiceResult<(String, PathBuf)> {
    let r = rel(raw.trim())?;
    if is_reserved_path(&r) {
        return Err(reserved(&r));
    }
    let abs = if follow_last {
        resolve_in_repo(repo, &r, false).map_err(ServiceError::invalid)?
    } else {
        let (parent, name) = match r.rsplit_once('/') {
            Some((p, n)) => (p, n),
            None => ("", r.as_str()),
        };
        resolve_in_repo(repo, parent, false)
            .map_err(ServiceError::invalid)?
            .join(name)
    };
    // A symlink inside the repo may still point into a reserved directory.
    if let Ok(root) = std::fs::canonicalize(repo) {
        if let Ok(inner) = abs.strip_prefix(&root) {
            let inner = inner.to_string_lossy().replace('\\', "/");
            if !inner.is_empty() && is_reserved_path(&inner) {
                return Err(reserved(&r));
            }
        }
    }
    Ok((r, abs))
}

fn read_capped(rel: &str, full: &Path, cap: u64) -> ServiceResult<Vec<u8>> {
    let meta = std::fs::metadata(full).map_err(|e| map_io(rel, e))?;
    if meta.is_dir() {
        return Err(ServiceError::invalid(format!("`{rel}` is a directory")));
    }
    if meta.len() > cap {
        return Err(ServiceError::invalid(format!(
            "`{rel}` is too large to open here ({} bytes; the limit is {cap})",
            meta.len()
        ))
        .with_detail(serde_json::json!({"path": rel, "size": meta.len(), "limit": cap})));
    }
    std::fs::read(full).map_err(|e| map_io(rel, e))
}

/// Replace `full` with `bytes` atomically: a temporary file in the same directory, then a
/// rename. The permissions of an existing file are kept.
fn write_atomic(rel: &str, full: &Path, bytes: &[u8]) -> ServiceResult<()> {
    use std::io::Write;
    let dir = full
        .parent()
        .ok_or_else(|| ServiceError::invalid(format!("`{rel}` has no parent directory")))?;
    std::fs::create_dir_all(dir).map_err(|e| map_io(rel, e))?;
    let name = full
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", crate::util::short_rand()));
    let existing = std::fs::metadata(full).ok();
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Some(m) = &existing {
            std::fs::set_permissions(&tmp, m.permissions())?;
        }
        std::fs::rename(&tmp, full)
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(map_io(rel, e));
    }
    Ok(())
}

/// Directory tree under `path` (default root) to `depth` levels (default 1). Entries respect
/// `.gitignore`; the project `out_dir` is shown at the top level but never expanded unless it
/// is the requested path itself (`children: null` = not expanded).
pub fn tree(
    repo: &Path,
    path: Option<&str>,
    depth: Option<u32>,
    out_dir: &str,
) -> ServiceResult<Vec<FileEntry>> {
    let (base_rel, base) = resolve(repo, path.unwrap_or(""))?;
    if !base.is_dir() {
        return Err(ServiceError::not_found(format!(
            "`{}` is not a directory",
            if base_rel.is_empty() { "." } else { &base_rel }
        )));
    }
    let depth = depth.unwrap_or(1).clamp(1, 8) as usize;
    let out_rel = out_dir.trim_matches('/').to_string();
    let inside_out = !out_rel.is_empty()
        && (base_rel == out_rel || base_rel.starts_with(&format!("{out_rel}/")));
    // `base` is canonical; compare against the canonical root when looking for `.gitignore`s.
    let canon_repo = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let repo = canon_repo.as_path();
    let mut root_children = walk_dir(repo, &base, &base_rel, depth, &out_rel, inside_out)?;
    // `out/` is gitignored by the template; show it (collapsed) at the top level anyway.
    if base_rel.is_empty() && !out_rel.is_empty() && !out_rel.contains('/') {
        let out_abs = repo.join(&out_rel);
        if out_abs.is_dir() && !root_children.iter().any(|e| e.path == out_rel) {
            root_children.push(FileEntry {
                path: out_rel.clone(),
                name: out_rel.clone(),
                kind: FileKind::Dir,
                size: None,
                children: None,
            });
            sort_entries(&mut root_children);
        }
    }
    Ok(root_children)
}

fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        let ka = (a.kind != FileKind::Dir, a.name.to_lowercase());
        let kb = (b.kind != FileKind::Dir, b.name.to_lowercase());
        ka.cmp(&kb)
    });
}

fn walk_dir(
    repo: &Path,
    dir: &Path,
    dir_rel: &str,
    depth: usize,
    out_rel: &str,
    inside_out: bool,
) -> ServiceResult<Vec<FileEntry>> {
    let mut walker = ignore::WalkBuilder::new(dir);
    walker
        .max_depth(Some(1))
        .hidden(false)
        .require_git(false)
        .git_ignore(!inside_out)
        .git_global(false)
        .git_exclude(!inside_out)
        .sort_by_file_name(|a, b| a.cmp(b));
    // `.gitignore` files above `dir` must apply too.
    if dir != repo {
        let mut parents: Vec<PathBuf> = vec![];
        let mut cur = dir.parent();
        while let Some(p) = cur {
            parents.push(p.to_path_buf());
            if p == repo {
                break;
            }
            cur = p.parent();
        }
        for p in parents.iter().rev() {
            let gi = p.join(".gitignore");
            if gi.is_file() && !inside_out {
                walker.add_ignore(gi);
            }
        }
    }
    let mut entries = Vec::new();
    for item in walker.build().flatten() {
        if item.depth() == 0 {
            continue;
        }
        let name = item.file_name().to_string_lossy().to_string();
        if ALWAYS_HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let path = if dir_rel.is_empty() {
            name.clone()
        } else {
            format!("{dir_rel}/{name}")
        };
        let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            let is_out = !out_rel.is_empty() && path == out_rel;
            let children = if depth > 1 && !is_out {
                Some(walk_dir(
                    repo,
                    item.path(),
                    &path,
                    depth - 1,
                    out_rel,
                    inside_out,
                )?)
            } else {
                None
            };
            entries.push(FileEntry {
                path,
                name,
                kind: FileKind::Dir,
                size: None,
                children,
            });
        } else {
            let size = item.metadata().ok().map(|m| m.len());
            entries.push(FileEntry {
                path,
                name,
                kind: FileKind::File,
                size,
                children: None,
            });
        }
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// Read a text file byte-exact (no newline normalisation); binary content and files over
/// [`MAX_TEXT_BYTES`] are `invalid`.
pub fn read(repo: &Path, path: &str) -> ServiceResult<FileContent> {
    let (r, full) = resolve(repo, path)?;
    let bytes = read_capped(&r, &full, MAX_TEXT_BYTES)?;
    let hash = sha256_hex(&bytes);
    let content = String::from_utf8(bytes)
        .map_err(|_| ServiceError::invalid(format!("`{r}` is not a UTF-8 text file")))?;
    Ok(FileContent {
        path: r,
        content,
        hash,
    })
}

/// Any file under the repo (at most [`MAX_BINARY_BYTES`]).
pub fn read_binary(repo: &Path, path: &str) -> ServiceResult<(String, Vec<u8>)> {
    let (r, full) = resolve(repo, path)?;
    let bytes = read_capped(&r, &full, MAX_BINARY_BYTES)?;
    Ok((r, bytes))
}

pub fn to_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Write a file byte-exact and atomically. With `expected_hash`, the current content must hash
/// to it (`conflict` otherwise; a missing file matches only an empty `expected_hash`).
pub fn write(
    repo: &Path,
    path: &str,
    content: &str,
    expected_hash: Option<&str>,
) -> ServiceResult<WriteResult> {
    let (r, full) = resolve_mut(repo, path, true)?;
    if full.is_dir() {
        return Err(ServiceError::invalid(format!("`{r}` is a directory")));
    }
    if let Some(expected) = expected_hash {
        let current = match std::fs::read(&full) {
            Ok(b) => Some(sha256_hex(&b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(map_io(&r, e)),
        };
        let matches = match &current {
            Some(h) => h == expected,
            None => expected.is_empty(),
        };
        if !matches {
            return Err(
                ServiceError::conflict(format!("`{r}` changed on disk since it was read"))
                    .with_detail(serde_json::json!({
                        "path": r,
                        "current_hash": current,
                        "expected_hash": expected,
                    })),
            );
        }
    }
    write_atomic(&r, &full, content.as_bytes())?;
    Ok(WriteResult {
        hash: sha256_hex(content.as_bytes()),
    })
}

pub fn create(repo: &Path, path: &str, is_dir: bool) -> ServiceResult<()> {
    let (r, full) = resolve_mut(repo, path, false)?;
    if std::fs::symlink_metadata(&full).is_ok() {
        return Err(ServiceError::conflict(format!("`{r}` already exists")));
    }
    if is_dir {
        std::fs::create_dir_all(&full).map_err(|e| map_io(&r, e))?;
    } else {
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| map_io(&r, e))?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full)
            .map_err(|e| map_io(&r, e))?;
    }
    Ok(())
}

/// Move to the OS trash when available, else delete. A symlink is removed itself; its target
/// is never touched.
pub fn delete(repo: &Path, path: &str) -> ServiceResult<()> {
    let (r, full) = resolve_mut(repo, path, false)?;
    let meta = std::fs::symlink_metadata(&full)
        .map_err(|_| ServiceError::not_found(format!("`{r}` does not exist")))?;
    if meta.file_type().is_symlink() {
        return std::fs::remove_file(&full).map_err(|e| map_io(&r, e));
    }
    if trash::delete(&full).is_ok() {
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::remove_dir_all(&full).map_err(|e| map_io(&r, e))?;
    } else {
        std::fs::remove_file(&full).map_err(|e| map_io(&r, e))?;
    }
    Ok(())
}

pub fn rename(repo: &Path, from: &str, to: &str) -> ServiceResult<()> {
    let (f, src) = resolve_mut(repo, from, false)?;
    let (t, dst) = resolve_mut(repo, to, false)?;
    if std::fs::symlink_metadata(&src).is_err() {
        return Err(ServiceError::not_found(format!("`{f}` does not exist")));
    }
    if std::fs::symlink_metadata(&dst).is_ok() {
        return Err(ServiceError::conflict(format!("`{t}` already exists")));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| map_io(&t, e))?;
    }
    std::fs::rename(&src, &dst).map_err(|e| map_io(&f, e))?;
    Ok(())
}

/// What one ripgrep search found.
#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    /// More matches exist than were returned.
    pub truncated: bool,
    /// ripgrep reported a problem (an unreadable file) but still found matches.
    pub warning: Option<String>,
}

const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_SEARCH_STDOUT: usize = 8 * 1024 * 1024;

/// Search file contents with ripgrep: the one search behind the UI's `fs_search` and the
/// agent's `fs.search`. `per_submatch` reports every match on a line with its column (the
/// UI); without it each matching line is reported once. The search stops at `max` hits and
/// after 30 seconds.
pub async fn grep(
    repo: &Path,
    pattern: &str,
    glob: Option<&str>,
    max: usize,
    per_submatch: bool,
) -> ServiceResult<SearchOutcome> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    if pattern.is_empty() {
        return Err(ServiceError::invalid("`pattern` must not be empty"));
    }
    let max = max.max(1);
    let rg = crate::util::find_tool("rg").ok_or_else(|| {
        ServiceError::internal("ripgrep (`rg`) is not installed, so search is unavailable")
    })?;
    let mut cmd = tokio::process::Command::new(rg);
    cmd.args(["--json", "--max-columns", "1000", "-e", pattern])
        .current_dir(repo)
        .env("PATH", crate::util::tool_path_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for h in ALWAYS_HIDDEN {
        cmd.arg("--glob").arg(format!("!{h}"));
    }
    if let Some(g) = glob {
        cmd.arg("--glob").arg(g);
    }
    cmd.arg("--").arg(".");
    let mut child = cmd
        .spawn()
        .map_err(|e| ServiceError::internal(format!("running rg: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ServiceError::internal("rg stdout not captured"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| ServiceError::internal("rg stderr not captured"))?;
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    });
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut truncated = false;
    let read = async {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut seen = 0usize;
        'lines: while let Ok(Some(line)) = lines.next_line().await {
            seen += line.len();
            if seen > MAX_SEARCH_STDOUT {
                truncated = true;
                break;
            }
            let v: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v["type"] != "match" {
                continue;
            }
            if hits.len() >= max {
                truncated = true;
                break;
            }
            let d = &v["data"];
            let path = d["path"]["text"]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("./")
                .to_string();
            let line_no = d["line_number"].as_u64().unwrap_or(0);
            let text = d["lines"]["text"].as_str().unwrap_or("").trim_end();
            let text = crate::util::truncate_bytes(text, 400).to_string();
            let cols: Vec<u64> = d["submatches"]
                .as_array()
                .map(|subs| {
                    subs.iter()
                        .map(|sm| sm["start"].as_u64().unwrap_or(0) + 1)
                        .collect()
                })
                .unwrap_or_default();
            let cols = if cols.is_empty() || !per_submatch {
                vec![cols.first().copied().unwrap_or(1)]
            } else {
                cols
            };
            for col in cols {
                if hits.len() >= max {
                    truncated = true;
                    break 'lines;
                }
                hits.push(SearchHit {
                    path: path.clone(),
                    line: line_no,
                    col,
                    text: text.clone(),
                });
            }
        }
    };
    let timed_out = tokio::time::timeout(SEARCH_TIMEOUT, read).await.is_err();
    if timed_out || truncated {
        let _ = child.start_kill();
    }
    let status = child.wait().await.ok();
    let stderr_text = err_task.await.unwrap_or_default();
    if timed_out {
        return Err(ServiceError::invalid(format!(
            "search timed out after {} s; narrow the pattern or add a glob",
            SEARCH_TIMEOUT.as_secs()
        )));
    }
    // rg exits 0 with matches, 1 with none, 2 on errors (bad regex, unreadable files).
    // Killing it after enough matches is not an error.
    let code = status.and_then(|s| s.code());
    let mut warning = None;
    if !truncated && code != Some(0) && code != Some(1) {
        let msg = stderr_text.trim();
        if hits.is_empty() {
            return Err(ServiceError::invalid(format!(
                "search failed: ripgrep failed (this is not \"no matches\"): {}",
                if msg.is_empty() {
                    format!("exit status {code:?}")
                } else {
                    crate::util::truncate_bytes(msg, 2000).to_string()
                }
            )));
        }
        warning = Some(crate::util::truncate_bytes(msg, 1000).to_string());
    }
    Ok(SearchOutcome {
        hits,
        truncated,
        warning,
    })
}

/// `fs_search`: every match with its column.
pub async fn search(
    repo: &Path,
    pattern: &str,
    glob: Option<&str>,
    max_results: Option<u32>,
) -> ServiceResult<Vec<SearchHit>> {
    let max = max_results.unwrap_or(200).max(1) as usize;
    Ok(grep(repo, pattern, glob, max, true).await?.hits)
}

/// A page of a text file as the agent reads it, or the fact that the file is binary.
#[derive(Debug, Clone)]
pub enum Page {
    Text {
        start_line: usize,
        end_line: usize,
        total_lines: usize,
        /// The lines, each prefixed with its number.
        content: String,
    },
    Binary {
        size: usize,
    },
}

/// The workspace-files service of a session: every file access of the UI's `fs_*` commands
/// and of the agent's file tools goes through it (DESIGN-REVIEW item 1). It confines paths
/// to the repository (plus, for reads, the session's spilled tool results and approved data
/// directories), reads, lists, searches with one ripgrep runner, and writes: editor saves
/// with hash conflict detection, agent proposals with create-new and no-symlink semantics.
#[derive(Debug, Clone)]
pub struct WorkspaceFiles {
    repo: PathBuf,
    /// The session's `artifacts/` directory (spilled tool results), readable by absolute path.
    artifacts_dir: Option<PathBuf>,
}

impl WorkspaceFiles {
    pub fn new(repo: &Path) -> WorkspaceFiles {
        WorkspaceFiles {
            repo: repo.to_path_buf(),
            artifacts_dir: None,
        }
    }

    pub fn with_artifacts(mut self, dir: PathBuf) -> WorkspaceFiles {
        self.artifacts_dir = Some(dir);
        self
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }

    pub fn resolve(&self, raw: &str) -> ServiceResult<(String, PathBuf)> {
        resolve(&self.repo, raw)
    }

    pub fn tree(
        &self,
        path: Option<&str>,
        depth: Option<u32>,
        out_dir: &str,
    ) -> ServiceResult<Vec<FileEntry>> {
        tree(&self.repo, path, depth, out_dir)
    }

    pub fn read(&self, path: &str) -> ServiceResult<FileContent> {
        read(&self.repo, path)
    }

    pub fn read_binary(&self, path: &str) -> ServiceResult<(String, Vec<u8>)> {
        read_binary(&self.repo, path)
    }

    pub fn write(
        &self,
        path: &str,
        content: &str,
        expected_hash: Option<&str>,
    ) -> ServiceResult<WriteResult> {
        write(&self.repo, path, content, expected_hash)
    }

    pub fn create(&self, path: &str, is_dir: bool) -> ServiceResult<()> {
        create(&self.repo, path, is_dir)
    }

    pub fn delete(&self, path: &str) -> ServiceResult<()> {
        delete(&self.repo, path)
    }

    pub fn rename(&self, from: &str, to: &str) -> ServiceResult<()> {
        rename(&self.repo, from, to)
    }

    pub async fn search(
        &self,
        pattern: &str,
        glob: Option<&str>,
        max: usize,
        per_submatch: bool,
    ) -> ServiceResult<SearchOutcome> {
        grep(&self.repo, pattern, glob, max, per_submatch).await
    }

    /// Build an agent edit proposal against the file as it is now.
    pub fn propose(
        &self,
        id: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> anyhow::Result<crate::tools::proposals::Proposal> {
        crate::tools::proposals::build(&self.repo, id, tool, args)
    }

    /// Write an accepted agent proposal (conflict detection by base hash; a create never
    /// overwrites; nothing is written through a symlink or into a reserved directory).
    pub fn apply(&self, p: &crate::tools::proposals::Proposal) -> anyhow::Result<()> {
        crate::tools::proposals::apply(&self.repo, p)
    }

    /// Resolve a path the agent wants to read: repo-relative (canonicalised, so a symlink
    /// cannot lead out of the repo except into one of `extra_roots`, the approved data
    /// directories), or an absolute path inside the session artifacts directory.
    pub fn resolve_for_agent(&self, raw: &str, extra_roots: &[PathBuf]) -> ServiceResult<PathBuf> {
        let p = Path::new(raw);
        if p.is_absolute() {
            let canon = std::fs::canonicalize(p)
                .map_err(|e| ServiceError::not_found(format!("`{raw}`: {e}")))?;
            if let Some(art) = &self.artifacts_dir {
                let art_canon = std::fs::canonicalize(art).unwrap_or_else(|_| art.clone());
                if canon.starts_with(&art_canon) {
                    return Ok(canon);
                }
            }
            return Err(ServiceError::invalid(format!(
                "`{raw}` is outside the repo and the session artifacts directory"
            )));
        }
        // The repo's `.env` holds API keys for the app. The agent's file tools never read
        // it, so a key cannot end up in a prompt.
        if normalize_rel_path(raw).is_ok_and(|n| n.eq_ignore_ascii_case(wb_net::secrets::ENV_FILE))
        {
            return Err(ServiceError::policy_denied(
                "`.env` holds API keys and is not readable through the file tools",
            ));
        }
        crate::util::resolve_in_roots(&self.repo, raw, true, extra_roots)
            .map_err(ServiceError::invalid)
    }

    /// A line-numbered page of a text file for the agent (`fs.read`).
    pub fn read_page(
        &self,
        raw: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
        extra_roots: &[PathBuf],
    ) -> ServiceResult<Page> {
        let full = self.resolve_for_agent(raw, extra_roots)?;
        let bytes = std::fs::read(&full).map_err(|e| map_io(raw, e))?;
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                return Ok(Page::Binary {
                    size: e.as_bytes().len(),
                })
            }
        };
        let total = text.lines().count();
        let start = start_line.unwrap_or(1).max(1);
        let end = end_line.unwrap_or(total).min(total);
        // A single line can be enormous (minified data); bound each one so a page of lines
        // is a page.
        const MAX_LINE: usize = 2000;
        let content: String = if start > end {
            String::new()
        } else {
            text.lines()
                .enumerate()
                .filter(|(i, _)| *i + 1 >= start && *i < end)
                .map(|(i, l)| {
                    if l.len() > MAX_LINE {
                        format!(
                            "{:>5}| {}...[line truncated, {} bytes]",
                            i + 1,
                            crate::util::truncate_bytes(l, MAX_LINE),
                            l.len()
                        )
                    } else {
                        format!("{:>5}| {l}", i + 1)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(Page::Text {
            start_line: start,
            end_line: end,
            total_lines: total,
            content,
        })
    }

    /// A flat, depth-limited listing for the agent (`fs.list`): repo-relative paths,
    /// directories with a trailing slash, at most 2000 entries.
    pub fn list(&self, raw: &str, depth: usize) -> ServiceResult<Vec<String>> {
        let canon_repo = std::fs::canonicalize(&self.repo).map_err(|e| map_io(raw, e))?;
        let root = resolve_in_repo(&self.repo, raw, true).map_err(ServiceError::invalid)?;
        if !root.is_dir() {
            return Err(ServiceError::invalid(format!("`{raw}` is not a directory")));
        }
        let mut entries = Vec::new();
        for e in walkdir::WalkDir::new(&root)
            .min_depth(1)
            .max_depth(depth.clamp(1, 6))
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| !ALWAYS_HIDDEN.contains(&e.file_name().to_string_lossy().as_ref()))
            .flatten()
        {
            let p = e.path().strip_prefix(&canon_repo).unwrap_or(e.path());
            let mut s = p.to_string_lossy().replace('\\', "/");
            if e.file_type().is_dir() {
                s.push('/');
            }
            entries.push(s);
            if entries.len() >= 2000 {
                break;
            }
        }
        Ok(entries)
    }
}

fn map_io(path: &str, e: std::io::Error) -> ServiceError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ServiceError::not_found(format!("`{path}`: {e}")),
        _ => ServiceError::io(format!("`{path}`: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fibration_protocol::ui::IpcErrorCode;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("scripts")).unwrap();
        std::fs::create_dir_all(r.join("analysis/__pycache__")).unwrap();
        std::fs::create_dir_all(r.join(".venv/bin")).unwrap();
        std::fs::create_dir_all(r.join("out/fit/r1/figures")).unwrap();
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join(".gitignore"), "out/\n.venv/\n__pycache__/\n*.pyc\n").unwrap();
        std::fs::write(r.join("scripts/a.py"), "print('hi')\n").unwrap();
        std::fs::write(r.join("scripts/b.pyc"), "x").unwrap();
        std::fs::write(r.join("analysis/__init__.py"), "").unwrap();
        std::fs::write(r.join("analysis/__pycache__/x.pyc"), "x").unwrap();
        std::fs::write(r.join(".venv/bin/python"), "").unwrap();
        std::fs::write(r.join("out/fit/r1/figures/f.png"), "png").unwrap();
        d
    }

    #[test]
    fn tree_respects_gitignore_and_shows_out_collapsed() {
        let d = repo();
        let t = tree(d.path(), None, Some(3), "out").unwrap();
        let names: Vec<&str> = t.iter().map(|e| e.path.as_str()).collect();
        assert!(names.contains(&"scripts"));
        assert!(names.contains(&"analysis"));
        assert!(names.contains(&".gitignore"));
        assert!(!names.contains(&".venv"), "{names:?}");
        assert!(!names.contains(&".git"));
        let out = t.iter().find(|e| e.path == "out").expect("out shown");
        assert_eq!(out.kind, FileKind::Dir);
        assert!(out.children.is_none(), "out is collapsed");
        let analysis = t.iter().find(|e| e.path == "analysis").unwrap();
        let kids: Vec<&str> = analysis
            .children
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(kids, vec!["analysis/__init__.py"]);
        let scripts = t.iter().find(|e| e.path == "scripts").unwrap();
        let kids: Vec<&str> = scripts
            .children
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(kids, vec!["scripts/a.py"], "pyc ignored");
        // Explicitly listing inside out/ works.
        let inside = tree(d.path(), Some("out/fit/r1/figures"), None, "out").unwrap();
        assert_eq!(inside[0].path, "out/fit/r1/figures/f.png");
        assert_eq!(inside[0].size, Some(3));
        assert_eq!(
            tree(d.path(), Some("nope"), None, "out").unwrap_err().code,
            IpcErrorCode::NotFound
        );
    }

    #[test]
    fn write_detects_conflicts() {
        let d = repo();
        let c = read(d.path(), "scripts/a.py").unwrap();
        assert_eq!(c.content, "print('hi')\n");
        let w = write(d.path(), "scripts/a.py", "x = 1\n", Some(&c.hash)).unwrap();
        assert_eq!(w.hash, sha256_hex(b"x = 1\n"));
        let err = write(d.path(), "scripts/a.py", "y\n", Some(&c.hash)).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::Conflict);
        assert_eq!(err.detail.unwrap()["path"], "scripts/a.py");
        write(d.path(), "scripts/a.py", "z\n", Some(&w.hash)).unwrap();
        assert_eq!(read(d.path(), "scripts/a.py").unwrap().content, "z\n");
        // New file: no expected hash, or an empty one.
        write(d.path(), "new/dir/f.py", "n\n", None).unwrap();
        let err = write(d.path(), "new/dir/g.py", "n\n", Some("abc")).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::Conflict);
        write(d.path(), "new/dir/g.py", "n\n", Some("")).unwrap();
        assert_eq!(
            read(d.path(), "../etc/passwd").unwrap_err().code,
            IpcErrorCode::Invalid
        );
        std::fs::write(d.path().join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(
            read(d.path(), "bin.dat").unwrap_err().code,
            IpcErrorCode::Invalid
        );
        assert_eq!(read_binary(d.path(), "bin.dat").unwrap().1.len(), 3);
    }

    #[test]
    fn create_rename_delete() {
        let d = repo();
        create(d.path(), "docs", true).unwrap();
        assert!(d.path().join("docs").is_dir());
        create(d.path(), "docs/x.md", false).unwrap();
        assert_eq!(
            create(d.path(), "docs/x.md", false).unwrap_err().code,
            IpcErrorCode::Conflict
        );
        rename(d.path(), "docs/x.md", "docs/y.md").unwrap();
        assert!(d.path().join("docs/y.md").is_file());
        delete(d.path(), "docs/y.md").unwrap();
        assert!(!d.path().join("docs/y.md").exists());
        assert_eq!(
            delete(d.path(), "docs/y.md").unwrap_err().code,
            IpcErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn search_reports_columns() {
        let d = repo();
        let hits = search(d.path(), "hi", None, None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "scripts/a.py");
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].col, 8);
        let none = search(d.path(), "hi", Some("*.md"), None).await.unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn writes_are_byte_exact_atomic_and_keep_permissions() {
        let d = repo();
        let crlf = "a = 1\r\nb = 2\r\n";
        let w = write(d.path(), "scripts/crlf.py", crlf, None).unwrap();
        assert_eq!(w.hash, sha256_hex(crlf.as_bytes()));
        let c = read(d.path(), "scripts/crlf.py").unwrap();
        assert_eq!(c.content, crlf, "no newline normalisation");
        assert_eq!(c.hash, w.hash);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = d.path().join("scripts/run.sh");
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            write(d.path(), "scripts/run.sh", "#!/bin/sh\necho hi\n", None).unwrap();
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755);
        }
        // No temporary files are left behind.
        let left: Vec<String> = std::fs::read_dir(d.path().join("scripts"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn size_caps_are_invalid() {
        let d = repo();
        let big = d.path().join("big.txt");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_TEXT_BYTES + 1).unwrap();
        assert_eq!(
            read(d.path(), "big.txt").unwrap_err().code,
            IpcErrorCode::Invalid
        );
        assert!(read_binary(d.path(), "big.txt").is_ok());
        f.set_len(MAX_BINARY_BYTES + 1).unwrap();
        assert_eq!(
            read_binary(d.path(), "big.txt").unwrap_err().code,
            IpcErrorCode::Invalid
        );
    }

    /// G4: the renderer's file commands cannot change `.git/` or `.fibration/`.
    #[test]
    fn mutations_under_git_and_fibration_are_denied_but_reads_work() {
        let d = repo();
        std::fs::write(d.path().join(".git/config"), "[core]\n").unwrap();
        std::fs::create_dir_all(d.path().join(".fibration")).unwrap();
        std::fs::write(d.path().join(".fibration/project.toml"), "[project]\n").unwrap();
        assert_eq!(read(d.path(), ".git/config").unwrap().content, "[core]\n");
        assert!(read(d.path(), ".fibration/project.toml").is_ok());
        for p in [
            ".git/config",
            ".git/hooks/pre-commit",
            ".GIT/config",
            ".fibration/project.toml",
            "sub/.git/config",
        ] {
            assert_eq!(
                write(d.path(), p, "x", None).unwrap_err().code,
                IpcErrorCode::PolicyDenied,
                "{p}"
            );
            assert_eq!(
                create(d.path(), p, false).unwrap_err().code,
                IpcErrorCode::PolicyDenied
            );
            assert_eq!(
                delete(d.path(), p).unwrap_err().code,
                IpcErrorCode::PolicyDenied
            );
            assert_eq!(
                rename(d.path(), "scripts/a.py", p).unwrap_err().code,
                IpcErrorCode::PolicyDenied
            );
            assert_eq!(
                rename(d.path(), p, "scripts/x").unwrap_err().code,
                IpcErrorCode::PolicyDenied
            );
        }
        assert_eq!(
            delete(d.path(), ".git").unwrap_err().code,
            IpcErrorCode::PolicyDenied
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join(".git/config")).unwrap(),
            "[core]\n"
        );
        assert!(d.path().join("scripts/a.py").is_file());
        // A symlink inside the repo that points into .git is no way around it.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path().join(".git"), d.path().join("g")).unwrap();
            assert_eq!(
                write(d.path(), "g/config", "x", None).unwrap_err().code,
                IpcErrorCode::PolicyDenied
            );
            assert_eq!(
                std::fs::read_to_string(d.path().join(".git/config")).unwrap(),
                "[core]\n"
            );
        }
    }

    /// G7: symlinks never lead a file command out of the repo.
    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_the_repo() {
        use std::os::unix::fs::symlink;
        let d = repo();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(outside.path(), d.path().join("ext")).unwrap();
        symlink(outside.path().join("secret.txt"), d.path().join("link.txt")).unwrap();
        for p in ["ext/secret.txt", "link.txt"] {
            assert_eq!(
                read(d.path(), p).unwrap_err().code,
                IpcErrorCode::Invalid,
                "{p}"
            );
            assert_eq!(
                read_binary(d.path(), p).unwrap_err().code,
                IpcErrorCode::Invalid
            );
            assert_eq!(
                write(d.path(), p, "pwned", None).unwrap_err().code,
                IpcErrorCode::Invalid
            );
        }
        assert_eq!(
            write(d.path(), "ext/new.txt", "x", None).unwrap_err().code,
            IpcErrorCode::Invalid
        );
        assert_eq!(
            create(d.path(), "ext/new.txt", false).unwrap_err().code,
            IpcErrorCode::Invalid
        );
        assert_eq!(
            create(d.path(), "ext/newdir", true).unwrap_err().code,
            IpcErrorCode::Invalid
        );
        assert_eq!(
            delete(d.path(), "ext/secret.txt").unwrap_err().code,
            IpcErrorCode::Invalid
        );
        assert_eq!(
            rename(d.path(), "scripts/a.py", "ext/a.py")
                .unwrap_err()
                .code,
            IpcErrorCode::Invalid
        );
        assert_eq!(
            rename(d.path(), "ext/secret.txt", "stolen.txt")
                .unwrap_err()
                .code,
            IpcErrorCode::Invalid
        );
        assert_eq!(
            tree(d.path(), Some("ext"), None, "out").unwrap_err().code,
            IpcErrorCode::Invalid
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
            "secret"
        );
        assert!(!outside.path().join("new.txt").exists());
        assert!(!outside.path().join("a.py").exists());
        // The tree lists the link but does not descend through it.
        let t = tree(d.path(), None, Some(3), "out").unwrap();
        let ext = t.iter().find(|e| e.path == "ext").unwrap();
        assert!(ext.children.as_ref().map(|c| c.is_empty()).unwrap_or(true));
        // Deleting the link removes the link only.
        delete(d.path(), "ext").unwrap();
        assert!(!d.path().join("ext").exists());
        assert!(outside.path().join("secret.txt").is_file());
        // A link to a file inside the repo is fine, and writes go through it.
        symlink(d.path().join("scripts/a.py"), d.path().join("alias.py")).unwrap();
        assert_eq!(read(d.path(), "alias.py").unwrap().content, "print('hi')\n");
        write(d.path(), "alias.py", "x = 2\n", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("scripts/a.py")).unwrap(),
            "x = 2\n"
        );
    }

    #[tokio::test]
    async fn search_reports_invalid_patterns() {
        let d = repo();
        let err = search(d.path(), "(unclosed", None, None).await.unwrap_err();
        assert_eq!(err.code, IpcErrorCode::Invalid);
    }
}
