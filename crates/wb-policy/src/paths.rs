// Moved from Fibration Lab `crates/core/src/util.rs` (the fixes for REVIEW G4 and G7).
//! Path confinement for policies whose targets are files under a root directory: reserved
//! directories, symlink-aware resolution, and the case-sensitivity probe.
//!
//! This is the only part of the crate that looks at the filesystem, and it only reads.

use crate::normalize_rel_path;
use std::path::{Path, PathBuf};

/// Whether a root-relative path is reserved: anything under `.git/` (at any depth) or under
/// one of `reserved_top` (top-level directory names such as `.fibration`), in any letter case.
/// Paths that do not normalise are reported as reserved.
pub fn is_reserved_path(rel: &str, reserved_top: &[&str]) -> bool {
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
        if c == ".git" || c == "git~1" || (i == 0 && reserved_top.iter().any(|r| c == *r)) {
            return true;
        }
    }
    false
}

/// Resolve a root-relative path to an absolute one that provably stays inside the root: the
/// deepest existing ancestor is canonicalised (following symlinks) and must lie under the
/// canonical root. With `must_exist`, a missing path is an error. The returned path is the
/// canonical ancestor joined with the not-yet-existing remainder. `"."` and `""` mean the root.
pub fn resolve_in_root(root: &Path, rel: &str, must_exist: bool) -> Result<PathBuf, String> {
    resolve_in_roots(root, rel, must_exist, &[])
}

/// Like [`resolve_in_root`], but a path that resolves (through a symlink) into one of
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

/// The rules for edits whose targets are files under a root.
#[derive(Debug, Clone)]
pub struct PathRules {
    root: PathBuf,
    reserved_top: Vec<String>,
}

/// Why a path may not be edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathDenied {
    /// Not a usable root-relative path, or it leaves the root (lexically or by symlink).
    Outside(String),
    /// Under a reserved directory.
    Reserved(String),
}

impl PathRules {
    /// `reserved_top`: top-level directory names that are never editable, lower case
    /// (`.fibration`). `.git` at any depth is always reserved.
    pub fn new<I, S>(root: &Path, reserved_top: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        PathRules {
            root: root.to_path_buf(),
            reserved_top: reserved_top.into_iter().map(Into::into).collect(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_reserved(&self, rel: &str) -> bool {
        let top: Vec<&str> = self.reserved_top.iter().map(String::as_str).collect();
        is_reserved_path(rel, &top)
    }

    /// Sentence for the system prompt.
    pub fn reserved_summary(&self) -> String {
        let mut dirs = vec![".git/".to_string()];
        dirs.extend(self.reserved_top.iter().map(|d| format!("{d}/")));
        format!("Paths under {} are never editable.", dirs.join(" and "))
    }

    fn reserved_message(&self, path: &str) -> String {
        let mut dirs = vec![".git/".to_string()];
        dirs.extend(self.reserved_top.iter().map(|d| format!("{d}/")));
        format!(
            "`{path}` is under {}, which {} never editable",
            dirs.join(" or "),
            if dirs.len() == 1 { "is" } else { "are" }
        )
    }

    /// Check a path an edit names. Returns every name the file goes by, normalised: the
    /// given path and, when it differs (a symlink in the way), the path it really resolves
    /// to. Both must pass the policy's lock check, so a link to a protected file is
    /// protected too.
    pub fn check_edit(&self, raw: &str) -> Result<Vec<String>, PathDenied> {
        let path = normalize_rel_path(raw).map_err(PathDenied::Outside)?;
        if self.is_reserved(&path) {
            return Err(PathDenied::Reserved(self.reserved_message(&path)));
        }
        let full = resolve_in_root(&self.root, &path, false).map_err(PathDenied::Outside)?;
        let mut names = vec![path.clone()];
        if let Ok(canon_root) = std::fs::canonicalize(&self.root) {
            if let Ok(real_rel) = full.strip_prefix(&canon_root) {
                let real = real_rel.to_string_lossy().replace('\\', "/");
                if !real.is_empty() && real != path {
                    if self.is_reserved(&real) {
                        return Err(PathDenied::Reserved(format!(
                            "`{path}` resolves to `{real}`, which is protected"
                        )));
                    }
                    names.push(real);
                }
            }
        }
        Ok(names)
    }
}
