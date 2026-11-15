//! Small shared helpers: timestamps, hashing, ids, path normalization.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// Current time as RFC 3339 UTC with second precision (`2026-10-16T18:15:02Z`).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Compact UTC stamp used in ids and file names (`20261016-181502`).
pub fn now_stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Six lowercase hex characters of randomness.
pub fn short_rand() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..6].to_string()
}

/// A fresh id of the form `<stamp>-<6 hex>`.
pub fn stamped_id() -> String {
    format!("{}-{}", now_stamp(), short_rand())
}

/// Hex SHA-256 of bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Normalize a repo-relative path: forward slashes, no `.` components, and reject any path
/// that escapes the root (`..`) or is absolute.
pub fn normalize_rel_path(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    if p.is_absolute() || path.starts_with('~') {
        return Err(format!(
            "path `{path}` is absolute; paths must be relative to the repo root"
        ));
    }
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            Component::ParentDir => {
                return Err(format!("path `{path}` escapes the repo root"));
            }
            _ => return Err(format!("path `{path}` is not a relative path")),
        }
    }
    if parts.is_empty() {
        return Err("empty path".to_string());
    }
    Ok(parts.join("/"))
}

/// Join a normalized relative path onto the repo root.
pub fn repo_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    Ok(root.join(normalize_rel_path(rel)?))
}

/// Rough token estimate (4 characters per token) used for context budgeting.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Truncate a string at a byte limit on a char boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Expand a leading `~` with `$HOME`.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    } else if p == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(p)
}

/// Shell-quote a string for use inside a remote `ssh` command string (POSIX sh).
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,~".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Quote a remote path for a POSIX shell while keeping a leading `~` or `~/` unquoted so the
/// remote shell still expands it (`~/my repo` becomes `~/'my repo'`).
pub fn sh_quote_path(p: &str) -> String {
    if p == "~" {
        return "~".to_string();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if rest.is_empty() {
            return "~/".to_string();
        }
        return format!("~/{}", sh_quote(rest));
    }
    if p.starts_with('~') {
        // `~user/...` must not expand: force quotes.
        return format!("'{}'", p.replace('\'', "'\\''"));
    }
    sh_quote(p)
}

/// Whether a repo-relative path is reserved and never editable by the agent or the renderer:
/// anything under `.git/` (at any depth) or the top-level `.analyst/`, in any letter case.
/// Paths that do not normalise are reported as reserved.
pub fn is_reserved_path(rel: &str) -> bool {
    let Ok(norm) = normalize_rel_path(rel) else {
        return true;
    };
    for (i, comp) in norm.split('/').enumerate() {
        // Trailing dots and spaces are ignored by some filesystems (`.git.`, `.git `), and
        // HFS+ ignores some zero-width code points inside names.
        let c: String = comp
            .trim_end_matches(['.', ' '])
            .to_lowercase()
            .chars()
            .filter(|ch| {
                !matches!(
                    *ch,
                    '\u{200c}' | '\u{200d}' | '\u{200e}' | '\u{200f}' | '\u{feff}'
                )
            })
            .collect();
        // `git~1` is the Windows 8.3 short name of `.git`.
        if c == ".git" || c == "git~1" || (i == 0 && c == ".analyst") {
            return true;
        }
    }
    false
}

/// Resolve a repo-relative path to an absolute one that provably stays inside the repository:
/// the deepest existing ancestor is canonicalised (following symlinks) and must lie under the
/// canonical root. With `must_exist`, a missing path is an error. The returned path is the
/// canonical ancestor joined with the not-yet-existing remainder. `"."` and `""` mean the root.
pub fn resolve_in_repo(root: &Path, rel: &str, must_exist: bool) -> Result<PathBuf, String> {
    resolve_in_roots(root, rel, must_exist, &[])
}

/// Like [`resolve_in_repo`], but a path that resolves (through a symlink) into one of
/// `extra_roots` (approved data directories) is also accepted. Use for reads only.
pub fn resolve_in_roots(
    root: &Path,
    rel: &str,
    must_exist: bool,
    extra_roots: &[PathBuf],
) -> Result<PathBuf, String> {
    let norm = if rel.is_empty() || rel == "." {
        String::new()
    } else {
        normalize_rel_path(rel)?
    };
    let canon_root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot resolve the repo root {}: {e}", root.display()))?;
    let full = if norm.is_empty() {
        canon_root.clone()
    } else {
        canon_root.join(&norm)
    };
    // Walk up to the deepest ancestor that exists (as anything, including a dangling link).
    let mut existing: &Path = &full;
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    while std::fs::symlink_metadata(existing).is_err() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return Err(format!("path `{rel}` cannot be resolved")),
        }
    }
    if must_exist && !rest.is_empty() {
        return Err(format!("`{rel}` does not exist"));
    }
    let canon = std::fs::canonicalize(existing)
        .map_err(|e| format!("`{rel}` cannot be resolved (broken symlink?): {e}"))?;
    let inside = canon.starts_with(&canon_root)
        || extra_roots.iter().any(|r| {
            std::fs::canonicalize(r)
                .map(|c| canon.starts_with(c))
                .unwrap_or(false)
        });
    if !inside {
        return Err(format!(
            "path `{rel}` resolves outside the repository (through a symlink)"
        ));
    }
    let mut out = canon;
    for name in rest.into_iter().rev() {
        out.push(name);
    }
    Ok(out)
}

/// Whether `path` itself is a symbolic link (not followed).
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Whether the filesystem holding `root` is case-insensitive. Probed without writing: an
/// existing directory entry must be reachable under a case-flipped name.
pub fn fs_case_insensitive(root: &Path) -> bool {
    fn flip(name: &str) -> Option<String> {
        let f: String = name
            .chars()
            .map(|c| {
                if c.is_lowercase() {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c.to_lowercase().next().unwrap_or(c)
                }
            })
            .collect();
        (f != name).then_some(f)
    }
    fn same(a: &Path, b: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
                (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
                _ => false,
            }
        }
        #[cfg(not(unix))]
        {
            a.exists() && b.exists()
        }
    }
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        candidates.extend(rd.flatten().take(32).map(|e| e.path()));
    }
    candidates.push(root.clone());
    for c in candidates {
        let (Some(parent), Some(name)) = (c.parent(), c.file_name().and_then(|n| n.to_str()))
        else {
            continue;
        };
        if let Some(f) = flip(name) {
            return same(&c, &parent.join(f));
        }
    }
    false
}

// ---- external tools ----

const EXTRA_TOOL_DIRS: &[&str] = &[
    "~/.local/bin",
    "~/.cargo/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
];

fn path_looks_minimal(path: Option<&OsString>) -> bool {
    let Some(path) = path else { return true };
    let system = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    dirs.is_empty()
        || dirs
            .iter()
            .all(|d| system.iter().any(|s| d == Path::new(s)))
}

/// `$SHELL -lc 'printf %s "$PATH"'` with a 3 s timeout. Apps started from Finder or a desktop
/// launcher do not inherit the shell's PATH.
fn login_shell_path() -> Option<OsString> {
    use std::io::Read;
    let shell = std::env::var_os("SHELL").filter(|s| !s.is_empty())?;
    let mut child = std::process::Command::new(shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let out = out.trim();
    (!out.is_empty()).then(|| OsString::from(out))
}

/// Directories searched for external tools: the process PATH, the login shell's PATH (imported
/// once, only when the process PATH looks minimal), then the well-known install locations.
pub fn tool_search_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let process = std::env::var_os("PATH");
        let mut dirs: Vec<PathBuf> = process
            .as_ref()
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default();
        if path_looks_minimal(process.as_ref()) {
            if let Some(p) = login_shell_path() {
                dirs.extend(std::env::split_paths(&p));
            }
        }
        dirs.extend(EXTRA_TOOL_DIRS.iter().map(|d| expand_tilde(d)));
        let mut seen = std::collections::HashSet::new();
        dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
        dirs
    })
}

/// The search directories as a `PATH` value, for child processes that spawn tools themselves.
pub fn tool_path_env() -> OsString {
    std::env::join_paths(tool_search_dirs())
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

fn is_executable_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file() || p.with_extension("exe").is_file()
    }
}

/// Absolute path of an external tool (`uv`, `rg`, `git`, `ssh`, `rsync`, `tar`, `scp`, ...),
/// or `None` when it is not installed in any search directory.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return is_executable_file(Path::new(name)).then(|| PathBuf::from(name));
    }
    tool_search_dirs()
        .iter()
        .map(|d| d.join(name))
        .find(|p| is_executable_file(p))
}

/// Like [`find_tool`], falling back to the bare name so that spawning reports a clear
/// "not found" error instead of failing here.
pub fn resolve_tool(name: &str) -> PathBuf {
    find_tool(name).unwrap_or_else(|| PathBuf::from(name))
}

/// [`resolve_tool`] as a string, for command lines built as `Vec<String>`.
pub fn tool(name: &str) -> String {
    resolve_tool(name).to_string_lossy().to_string()
}

/// Keep the last `max` bytes of a string (on a char boundary), marking the cut.
pub fn tail_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("...[truncated]...{}", &s[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_path_quoting_keeps_tilde_expansion() {
        assert_eq!(sh_quote_path("~/analyst/repo"), "~/analyst/repo");
        assert_eq!(sh_quote_path("~/analyst/my repo"), "~/'analyst/my repo'");
        assert_eq!(sh_quote_path("/data/my repo"), "'/data/my repo'");
        assert_eq!(sh_quote_path("~"), "~");
        assert_eq!(sh_quote_path("~root/x"), "'~root/x'");
    }

    #[test]
    fn reserved_paths() {
        for p in [
            ".git/config",
            ".GIT/hooks/pre-commit",
            ".git",
            "./.git/x",
            ".analyst/project.toml",
            ".Analyst/project.toml",
            "sub/.git/config",
            ".git./config",
            "../x",
            "/abs",
        ] {
            assert!(is_reserved_path(p), "{p}");
        }
        for p in [
            "scripts/a.py",
            ".gitignore",
            "analysis/.analyst/x",
            "git/config",
            ".github/x",
        ] {
            assert!(!is_reserved_path(p), "{p}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_in_repo_rejects_symlink_escapes() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "s").unwrap();
        std::fs::create_dir_all(repo.path().join("scripts")).unwrap();
        std::fs::write(repo.path().join("scripts/a.py"), "a").unwrap();
        symlink(outside.path(), repo.path().join("link")).unwrap();
        symlink(
            outside.path().join("secret.txt"),
            repo.path().join("scripts/s.txt"),
        )
        .unwrap();
        symlink(repo.path().join("scripts"), repo.path().join("inner")).unwrap();
        symlink("/nonexistent/x", repo.path().join("dangling")).unwrap();
        let root = std::fs::canonicalize(repo.path()).unwrap();

        assert_eq!(
            resolve_in_repo(repo.path(), "scripts/a.py", true).unwrap(),
            root.join("scripts/a.py")
        );
        assert_eq!(
            resolve_in_repo(repo.path(), "new/dir/f.py", false).unwrap(),
            root.join("new/dir/f.py")
        );
        assert!(resolve_in_repo(repo.path(), "new/dir/f.py", true).is_err());
        assert!(resolve_in_repo(repo.path(), "link/secret.txt", true).is_err());
        assert!(resolve_in_repo(repo.path(), "link/new.txt", false).is_err());
        assert!(resolve_in_repo(repo.path(), "scripts/s.txt", true).is_err());
        assert!(resolve_in_repo(repo.path(), "dangling", false).is_err());
        assert!(resolve_in_repo(repo.path(), "../x", false).is_err());
        // A symlink that stays inside the repo is fine and resolves to its target.
        assert_eq!(
            resolve_in_repo(repo.path(), "inner/a.py", true).unwrap(),
            root.join("scripts/a.py")
        );
        assert_eq!(resolve_in_repo(repo.path(), ".", true).unwrap(), root);
        // Approved extra roots admit reads through the link.
        assert!(resolve_in_roots(
            repo.path(),
            "link/secret.txt",
            true,
            &[outside.path().to_path_buf()]
        )
        .is_ok());
    }

    #[test]
    fn tools_resolve_and_minimal_path_detection() {
        assert!(path_looks_minimal(None));
        assert!(path_looks_minimal(Some(&OsString::from("/usr/bin:/bin"))));
        assert!(!path_looks_minimal(Some(&OsString::from(
            "/usr/bin:/home/u/.local/bin"
        ))));
        let git = resolve_tool("git");
        assert!(git.is_absolute(), "{git:?}");
        assert_eq!(
            resolve_tool("definitely-not-a-tool-xyz"),
            PathBuf::from("definitely-not-a-tool-xyz")
        );
        assert!(find_tool("definitely-not-a-tool-xyz").is_none());
        assert!(!tool_path_env().is_empty());
    }

    #[test]
    fn case_probe_matches_this_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("probe.txt"), "x").unwrap();
        let insensitive = dir.path().join("PROBE.TXT").exists();
        assert_eq!(fs_case_insensitive(dir.path()), insensitive);
    }

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize_rel_path("./a/b.py").unwrap(), "a/b.py");
        assert!(normalize_rel_path("../x").is_err());
        assert!(normalize_rel_path("/etc/passwd").is_err());
        assert!(normalize_rel_path("a/../../b").is_err());
        assert_eq!(normalize_rel_path("a/./b").unwrap(), "a/b");
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("abc"), "abc");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "héllo";
        assert_eq!(truncate_bytes(s, 2), "h");
        assert_eq!(truncate_bytes(s, 3), "hé");
        assert_eq!(truncate_bytes(s, 100), s);
    }
}
