//! Checkpoints (ARCHITECTURE 5.9): the working tree (tracked and untracked, not ignored) is
//! written to a tree object through a temporary index (`GIT_INDEX_FILE`) and stored under
//! `refs/analyst/checkpoints/<id>`. The user's index, branches, and history are untouched.
//!
//! The temporary index is seeded from the user's index so unchanged files are not re-hashed,
//! and files above [`MAX_CHECKPOINT_FILE_BYTES`] or under a data directory are left out (they
//! are neither stored nor touched by a restore).
//!
//! Every `git` subprocess started by the core goes through [`git_command`], which neutralises
//! repository-controlled code execution (`core.fsmonitor`, hooks, `diff.external`).

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub const REF_PREFIX: &str = "refs/analyst/checkpoints/";

/// Files larger than this are not copied into the object store by checkpoints.
pub const MAX_CHECKPOINT_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Checkpoints older than this are pruned.
pub const MAX_CHECKPOINT_AGE_DAYS: u64 = 30;

/// Configuration overrides passed to every git invocation: a repository's own config must
/// never make a read-only git command run a program.
pub const GIT_HARDENING: [&str; 6] = [
    "-c",
    "core.fsmonitor=",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "diff.external=",
];

/// Full argument list for a git invocation: hardening overrides, then `args`, with
/// `--no-ext-diff --no-textconv` inserted after a `diff`/`show`/`log` subcommand.
pub fn hardened_args(args: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = GIT_HARDENING.iter().map(|s| s.to_string()).collect();
    for (i, a) in args.iter().enumerate() {
        v.push(a.to_string());
        if i == 0 && matches!(*a, "diff" | "show" | "log") {
            v.push("--no-ext-diff".into());
            v.push("--no-textconv".into());
        }
    }
    v
}

/// A `git` command in `repo` with the hardening overrides and the analyst identity applied.
pub fn git_command(repo: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(crate::util::resolve_tool("git"));
    cmd.args(hardened_args(args)).current_dir(repo);
    cmd.env("GIT_AUTHOR_NAME", "analyst")
        .env("GIT_AUTHOR_EMAIL", "analyst@localhost")
        .env("GIT_COMMITTER_NAME", "analyst")
        .env("GIT_COMMITTER_EMAIL", "analyst@localhost")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null());
    cmd
}

fn git_dir(repo: &Path) -> Result<PathBuf> {
    let out = git(repo, &["rev-parse", "--git-dir"], None)?;
    let p = PathBuf::from(out.trim());
    Ok(if p.is_absolute() { p } else { repo.join(p) })
}

fn git_bytes(repo: &Path, args: &[&str], env: Option<&[(&str, &str)]>) -> Result<Vec<u8>> {
    let mut cmd = git_command(repo, args);
    if let Some(env) = env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    let out = cmd
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Run git in the repo; optional extra environment. Returns stdout.
pub fn git(repo: &Path, args: &[&str], env: Option<&[(&str, &str)]>) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_bytes(repo, args, env)?).to_string())
}

/// A temporary index file inside `.git`, seeded from the user's index, removed on drop.
struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    fn new(repo: &Path) -> Result<Self> {
        let dir = git_dir(repo)?;
        let path = dir.join(format!("analyst-index-{}", crate::util::short_rand()));
        let user_index = dir.join("index");
        if user_index.is_file() {
            // Seeding keeps the stat cache, so only files that changed are hashed again.
            std::fs::copy(&user_index, &path)
                .with_context(|| format!("seeding {}", path.display()))?;
            // Git trusts a cached stat entry only when the file is older than the index
            // itself; anything as new as the index is hashed again. A copy has a fresh
            // timestamp, which would make an edit that kept a file's size, made in the same
            // instant the user's index was written, look unchanged. Keep the original time.
            let written = std::fs::metadata(&user_index)
                .and_then(|m| m.modified())
                .with_context(|| format!("reading the time of {}", user_index.display()))?;
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(written))
                .with_context(|| format!("seeding {}", path.display()))?;
        }
        Ok(TempIndex { path })
    }
    fn env(&self) -> Vec<(&str, String)> {
        vec![("GIT_INDEX_FILE", self.path.display().to_string())]
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(format!("{}.lock", self.path.display()));
    }
}

fn with_env<'a>(env: &'a [(&'a str, String)]) -> Vec<(&'a str, &'a str)> {
    env.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// Repo-relative directories whose contents checkpoints leave alone: `data/` plus the
/// project's `data_dirs` that lie inside the repo.
pub fn excluded_dirs(repo: &Path) -> Vec<String> {
    let mut dirs = vec!["data".to_string()];
    if let Ok(project) = crate::config::load_project(repo) {
        let mut all = project.project.data_dirs.clone();
        for t in project.targets.values() {
            all.extend(t.data_dirs.clone().unwrap_or_default());
        }
        for d in all {
            if let Ok(n) = crate::util::normalize_rel_path(&d) {
                if !dirs.contains(&n) {
                    dirs.push(n);
                }
            }
        }
    }
    dirs
}

fn under(path: &str, dirs: &[String]) -> bool {
    dirs.iter().any(|d| {
        path == d
            || path
                .strip_prefix(d.as_str())
                .is_some_and(|r| r.starts_with('/'))
    })
}

/// Bring the temp index up to date with the working tree, skipping data directories and
/// large files. Returns the large files that were skipped.
fn load_worktree(repo: &Path, env: &[(&str, &str)], exclude: &[String]) -> Result<Vec<String>> {
    let listed = git_bytes(
        repo,
        &[
            "ls-files",
            "-z",
            "--others",
            "--modified",
            "--deleted",
            "--exclude-standard",
        ],
        Some(env),
    )?;
    let mut input: Vec<u8> = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        if !seen.insert(raw) {
            continue;
        }
        let path = String::from_utf8_lossy(raw);
        if under(&path, exclude) {
            continue;
        }
        #[cfg(unix)]
        let full = {
            use std::os::unix::ffi::OsStrExt;
            repo.join(std::ffi::OsStr::from_bytes(raw))
        };
        #[cfg(not(unix))]
        let full = repo.join(path.as_ref());
        match std::fs::symlink_metadata(&full) {
            Ok(m) if m.is_file() && m.len() > MAX_CHECKPOINT_FILE_BYTES => {
                skipped.push(path.to_string());
                continue;
            }
            Ok(m) if m.is_dir() => continue, // nested repository
            _ => {}
        }
        input.extend_from_slice(raw);
        input.push(0);
    }
    update_index(repo, env, &["--add", "--remove"], input)?;
    Ok(skipped)
}

/// `git update-index <flags> -z --stdin` over NUL-separated paths in the temp index.
fn update_index(repo: &Path, env: &[(&str, &str)], flags: &[&str], input: Vec<u8>) -> Result<()> {
    use std::io::Write;
    if input.is_empty() {
        return Ok(());
    }
    let mut args = vec!["update-index"];
    args.extend_from_slice(flags);
    args.extend_from_slice(&["-z", "--stdin"]);
    let mut cmd = git_command(repo, &args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("running git update-index")?;
    // update-index prints nothing on stdout, so writing all of stdin first cannot deadlock.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&input)
            .context("feeding git update-index")?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "git update-index failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Write the current working tree (tracked + untracked, excluding ignored files, data
/// directories and large files) to a tree object without touching the user's index.
/// Returns the tree sha.
pub fn worktree_tree(repo: &Path) -> Result<String> {
    worktree_tree_excluding(repo, &excluded_dirs(repo))
}

/// [`worktree_tree`] with an explicit list of repo-relative directories to leave out.
pub fn worktree_tree_excluding(repo: &Path, exclude: &[String]) -> Result<String> {
    Ok(worktree_tree_without(repo, exclude, &[])?.tree)
}

/// A working-tree snapshot and what it leaves out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The tree object id.
    pub tree: String,
    /// Changed or untracked files above [`MAX_CHECKPOINT_FILE_BYTES`], which are not in the
    /// tree (a tracked one keeps the content the user's index has for it).
    pub skipped_large: Vec<String>,
}

/// The checkpoint tree with `drop` removed from it: repo-relative files or directories
/// (literal paths, not patterns) that must not be part of the snapshot even when they are
/// tracked. Remote sync uses this to keep `.env` and the host-owned directories out of what
/// it pushes.
pub fn worktree_tree_without(repo: &Path, exclude: &[String], drop: &[String]) -> Result<Snapshot> {
    let idx = TempIndex::new(repo)?;
    let env = idx.env();
    let mut env = with_env(&env);
    env.push(("GIT_LITERAL_PATHSPECS", "1"));
    let skipped_large = load_worktree(repo, &env, exclude)?;
    if !drop.is_empty() {
        let mut args = vec!["ls-files", "-z", "--"];
        args.extend(drop.iter().map(String::as_str));
        let listed = git_bytes(repo, &args, Some(&env))?;
        update_index(repo, &env, &["--force-remove"], listed)?;
    }
    let tree = git(repo, &["write-tree"], Some(&env))?;
    Ok(Snapshot {
        tree: tree.trim().to_string(),
        skipped_large,
    })
}

/// A parentless commit of `tree` with a fixed date, so the same tree always gives the same
/// commit id. It carries no history: pushing it sends the tree and nothing else. No ref is
/// created.
pub fn commit_tree_detached(repo: &Path, tree: &str, message: &str) -> Result<String> {
    const DATE: &str = "2000-01-01T00:00:00+0000";
    let env = [("GIT_AUTHOR_DATE", DATE), ("GIT_COMMITTER_DATE", DATE)];
    Ok(
        git(repo, &["commit-tree", tree, "-m", message], Some(&env))?
            .trim()
            .to_string(),
    )
}

/// A checkpoint id that is unique across conversations, processes, and restarts:
/// `<conversation id>-<stamp>-<6 hex>` (characters outside `[A-Za-z0-9._-]` become `_`).
pub fn new_id(conversation: &str) -> String {
    let safe: String = conversation
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}-{}", crate::util::stamped_id())
}

/// Create a checkpoint `refs/analyst/checkpoints/<id>`; returns the ref name. Fails when the
/// ref already exists: an undo point is never overwritten.
pub fn create(repo: &Path, id: &str) -> Result<String> {
    let tree = worktree_tree(repo)?;
    create_from_tree(repo, id, &tree)
}

/// [`create`] with a label saying why the checkpoint was taken (`before switching the deck
/// to code mode`, `export pptx`); [`history`] shows it.
pub fn create_labeled(repo: &Path, id: &str, label: &str) -> Result<String> {
    let tree = worktree_tree(repo)?;
    commit_checkpoint(repo, id, &tree, Some(label))
}

/// [`create`] for a tree that was already written with [`worktree_tree`].
pub fn create_from_tree(repo: &Path, id: &str, tree: &str) -> Result<String> {
    commit_checkpoint(repo, id, tree, None)
}

fn commit_checkpoint(repo: &Path, id: &str, tree: &str, label: Option<&str>) -> Result<String> {
    let head = git(repo, &["rev-parse", "--verify", "-q", "HEAD"], None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let msg = match label.map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => format!("analyst checkpoint {id}\n\n{}", l.replace('\n', " ")),
        None => format!("analyst checkpoint {id}"),
    };
    let mut args = vec!["commit-tree", tree, "-m", msg.as_str()];
    if let Some(h) = head.as_deref() {
        args.push("-p");
        args.push(h);
    }
    let commit = git(repo, &args, None)?.trim().to_string();
    let r = format!("{REF_PREFIX}{id}");
    // An empty old value makes update-ref fail when the ref exists.
    git(repo, &["update-ref", &r, &commit, ""], None)
        .map_err(|e| anyhow!("checkpoint `{id}` already exists or cannot be written: {e}"))?;
    Ok(r)
}

/// Tree sha of a checkpoint.
pub fn tree_of(repo: &Path, id: &str) -> Result<String> {
    let r = format!("{REF_PREFIX}{id}");
    Ok(git(repo, &["rev-parse", &format!("{r}^{{tree}}")], None)?
        .trim()
        .to_string())
}

/// Restore the working tree to a checkpoint: files added since are removed, changed files are
/// rewritten. Ignored files, data directories, large files, and the user's index are untouched.
pub fn restore(repo: &Path, id: &str) -> Result<()> {
    let tree = tree_of(repo, id)?;
    let idx = TempIndex::new(repo)?;
    let env = idx.env();
    let env = with_env(&env);
    // Load the current working tree into the temp index so read-tree knows what to remove.
    load_worktree(repo, &env, &excluded_dirs(repo))?;
    git(repo, &["read-tree", "-u", "--reset", &tree], Some(&env))?;
    Ok(())
}

/// Restore only `paths` (repo-relative files or directories, literal) to a checkpoint: files
/// under them that the checkpoint does not have are removed, the others are rewritten. The
/// rest of the working tree and the user's index are untouched.
pub fn restore_paths(repo: &Path, id: &str, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let tree = tree_of(repo, id)?;
    let idx = TempIndex::new(repo)?;
    let env = idx.env();
    let mut env = with_env(&env);
    env.push(("GIT_LITERAL_PATHSPECS", "1"));
    // The temp index knows every current file, so files the checkpoint lacks are removed.
    load_worktree(repo, &env, &excluded_dirs(repo))?;
    let mut args = vec![
        "restore",
        "--no-overlay",
        "--staged",
        "--worktree",
        "--source",
        tree.as_str(),
        "--",
    ];
    args.extend(paths.iter().map(String::as_str));
    git(repo, &args, Some(&env))?;
    Ok(())
}

/// One checkpoint as the history shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointInfo {
    pub id: String,
    pub commit: String,
    /// Seconds since the epoch.
    pub created: u64,
    /// Why it was taken, when the creator said (`create_labeled`).
    pub label: Option<String>,
}

/// Every checkpoint, oldest first, with its date and label.
pub fn history(repo: &Path) -> Result<Vec<CheckpointInfo>> {
    let out = git(
        repo,
        &[
            "for-each-ref",
            "--sort=committerdate",
            "--format=%(refname)%00%(objectname)%00%(committerdate:unix)%00%(contents:body)%1e",
            REF_PREFIX,
        ],
        None,
    )?;
    Ok(out
        .split('\u{1e}')
        .filter_map(|rec| {
            let mut it = rec.trim_start_matches('\n').split('\0');
            let r = it.next()?;
            let commit = it.next()?.to_string();
            let created = it.next().and_then(|d| d.trim().parse().ok()).unwrap_or(0);
            let label = it
                .next()
                .map(|b| b.trim().to_string())
                .filter(|b| !b.is_empty());
            Some(CheckpointInfo {
                id: r.strip_prefix(REF_PREFIX)?.to_string(),
                commit,
                created,
                label,
            })
        })
        .collect())
}

fn list_with_dates(repo: &Path) -> Result<Vec<(String, String, u64)>> {
    let out = git(
        repo,
        &[
            "for-each-ref",
            "--sort=committerdate",
            "--format=%(refname) %(objectname) %(committerdate:unix)",
            REF_PREFIX,
        ],
        None,
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut it = l.split(' ');
            let r = it.next()?;
            let sha = it.next()?;
            let date = it.next().and_then(|d| d.parse().ok()).unwrap_or(0);
            Some((
                r.strip_prefix(REF_PREFIX)?.to_string(),
                sha.to_string(),
                date,
            ))
        })
        .collect())
}

/// Checkpoint ids with their commits, oldest first (creation order by committer date).
pub fn list(repo: &Path) -> Result<Vec<(String, String)>> {
    Ok(list_with_dates(repo)?
        .into_iter()
        .map(|(id, sha, _)| (id, sha))
        .collect())
}

/// Delete a checkpoint ref.
pub fn delete(repo: &Path, id: &str) -> Result<()> {
    git(
        repo,
        &["update-ref", "-d", &format!("{REF_PREFIX}{id}")],
        None,
    )?;
    Ok(())
}

/// Keep only the newest `keep` checkpoints, and none older than
/// [`MAX_CHECKPOINT_AGE_DAYS`]. Returns the ids removed.
pub fn prune(repo: &Path, keep: usize) -> Result<Vec<String>> {
    prune_by(
        repo,
        keep,
        std::time::Duration::from_secs(MAX_CHECKPOINT_AGE_DAYS * 86_400),
    )
}

/// Prune by count and by age.
pub fn prune_by(repo: &Path, keep: usize, max_age: std::time::Duration) -> Result<Vec<String>> {
    let all = list_with_dates(repo)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cutoff = now.saturating_sub(max_age.as_secs());
    let over = all.len().saturating_sub(keep);
    let mut removed = Vec::new();
    for (i, (id, _, date)) in all.iter().enumerate() {
        if i < over || *date <= cutoff {
            delete(repo, id)?;
            removed.push(id.clone());
        }
    }
    Ok(removed)
}

/// Paths that differ between two tree objects.
pub fn changed_files(repo: &Path, from_tree: &str, to_tree: &str) -> Result<Vec<String>> {
    let out = git(
        repo,
        &["diff-tree", "-r", "--name-only", from_tree, to_tree],
        None,
    )?;
    Ok(out
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"], None).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "out/\n").unwrap();
        std::fs::write(dir.path().join("a.py"), "a = 1\n").unwrap();
        git(dir.path(), &["add", "-A"], None).unwrap();
        git(dir.path(), &["commit", "-q", "-m", "init"], None).unwrap();
        dir
    }

    #[test]
    fn checkpoint_restore_and_prune() {
        let repo = temp_repo();
        let r = repo.path();
        std::fs::write(r.join("untracked.py"), "u = 1\n").unwrap();
        std::fs::create_dir_all(r.join("out")).unwrap();
        std::fs::write(r.join("out/ignored.txt"), "ignored\n").unwrap();
        let index_before = std::fs::read(r.join(".git/index")).unwrap();

        let cp = create(r, "t1").unwrap();
        assert_eq!(cp, "refs/analyst/checkpoints/t1");
        assert_eq!(
            std::fs::read(r.join(".git/index")).unwrap(),
            index_before,
            "user index untouched"
        );
        // `git status` must still show untracked.py as untracked (not staged).
        let st = git(r, &["status", "--porcelain"], None).unwrap();
        assert!(st.contains("?? untracked.py"), "{st}");
        assert!(
            !r.join(".git").read_dir().unwrap().any(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("analyst-index-")
            }),
            "temp index removed"
        );

        // Mutate: change a.py, delete untracked.py, add new.py.
        std::fs::write(r.join("a.py"), "a = 2\n").unwrap();
        std::fs::remove_file(r.join("untracked.py")).unwrap();
        std::fs::write(r.join("new.py"), "n = 1\n").unwrap();
        let t2 = worktree_tree(r).unwrap();
        let t1 = tree_of(r, "t1").unwrap();
        let mut changed = changed_files(r, &t1, &t2).unwrap();
        changed.sort();
        assert_eq!(changed, vec!["a.py", "new.py", "untracked.py"]);

        restore(r, "t1").unwrap();
        assert_eq!(std::fs::read_to_string(r.join("a.py")).unwrap(), "a = 1\n");
        assert_eq!(
            std::fs::read_to_string(r.join("untracked.py")).unwrap(),
            "u = 1\n"
        );
        assert!(
            !r.join("new.py").exists(),
            "file added after checkpoint removed"
        );
        assert_eq!(
            std::fs::read_to_string(r.join("out/ignored.txt")).unwrap(),
            "ignored\n"
        );

        create(r, "t2").unwrap();
        create(r, "t3").unwrap();
        assert_eq!(list(r).unwrap().len(), 3);
        let removed = prune(r, 2).unwrap();
        assert_eq!(removed, vec!["t1"]);
        let ids: Vec<String> = list(r).unwrap().into_iter().map(|(i, _)| i).collect();
        assert_eq!(ids, vec!["t2", "t3"]);
        assert!(
            git(r, &["log", "--oneline"], None).unwrap().lines().count() == 1,
            "history untouched"
        );
    }

    /// B11: a second conversation or process must never overwrite an earlier undo point.
    #[test]
    fn ids_are_unique_and_refs_are_never_overwritten() {
        let repo = temp_repo();
        let r = repo.path();
        let a = new_id("conv/1");
        let b = new_id("conv/1");
        assert_ne!(a, b);
        assert!(a.starts_with("conv_1-"));
        create(r, &a).unwrap();
        let first = list(r).unwrap();
        std::fs::write(r.join("a.py"), "a = 99\n").unwrap();
        let err = create(r, &a).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(list(r).unwrap(), first, "the existing ref is untouched");
        create(r, &b).unwrap();
        assert_eq!(list(r).unwrap().len(), 2);
    }

    /// A8: data directories and large files stay out of the object store, and a restore
    /// leaves them alone.
    #[test]
    fn data_dirs_and_large_files_are_not_checkpointed() {
        let repo = temp_repo();
        let r = repo.path();
        std::fs::create_dir_all(r.join("data")).unwrap();
        std::fs::write(r.join("data/big.csv"), "1,2,3\n").unwrap();
        let big = vec![b'x'; (MAX_CHECKPOINT_FILE_BYTES + 1) as usize];
        std::fs::write(r.join("huge.bin"), &big).unwrap();
        std::fs::write(r.join("small.py"), "s = 1\n").unwrap();
        create(r, "c1").unwrap();
        let files = git(
            r,
            &["ls-tree", "-r", "--name-only", &format!("{REF_PREFIX}c1")],
            None,
        )
        .unwrap();
        let files: Vec<&str> = files.lines().collect();
        assert!(files.contains(&"small.py"), "{files:?}");
        assert!(!files.contains(&"data/big.csv"), "{files:?}");
        assert!(!files.contains(&"huge.bin"), "{files:?}");
        // Later changes to the excluded files survive a restore; other changes are undone.
        std::fs::write(r.join("data/big.csv"), "9,9,9\n").unwrap();
        std::fs::write(r.join("small.py"), "s = 2\n").unwrap();
        std::fs::write(r.join("data/new.csv"), "n\n").unwrap();
        restore(r, "c1").unwrap();
        assert_eq!(
            std::fs::read_to_string(r.join("small.py")).unwrap(),
            "s = 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(r.join("data/big.csv")).unwrap(),
            "9,9,9\n"
        );
        assert!(r.join("data/new.csv").is_file());
        assert_eq!(
            std::fs::metadata(r.join("huge.bin")).unwrap().len(),
            big.len() as u64
        );
    }

    #[test]
    fn prune_by_age() {
        let repo = temp_repo();
        let r = repo.path();
        create(r, "old").unwrap();
        create(r, "new").unwrap();
        assert!(
            prune(r, 20).unwrap().is_empty(),
            "fresh checkpoints are kept"
        );
        let removed = prune_by(r, 20, std::time::Duration::from_secs(0)).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(list(r).unwrap().is_empty());
    }

    /// G4: a repository's own configuration must not make git run a program.
    #[test]
    fn git_ignores_repo_configured_programs() {
        let repo = temp_repo();
        let r = repo.path();
        let marker = r.join("pwned");
        let script = r.join("evil.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let s = script.display().to_string();
        for key in ["diff.external", "core.fsmonitor"] {
            git(r, &["config", key, &s], None).unwrap();
        }
        std::fs::write(r.join("a.py"), "a = 5\n").unwrap();
        let d = git(r, &["diff", "--no-color"], None).unwrap();
        assert!(d.contains("+a = 5"), "{d}");
        git(r, &["status", "--porcelain"], None).unwrap();
        worktree_tree(r).unwrap();
        assert!(!marker.exists(), "a configured program was executed");
        assert_eq!(
            hardened_args(&["diff", "--", "x"])[6..],
            ["diff", "--no-ext-diff", "--no-textconv", "--", "x"]
        );
    }
}
