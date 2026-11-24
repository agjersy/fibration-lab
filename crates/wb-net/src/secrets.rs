//! Secret keys (provider API keys and gateway tokens).
//!
//! A key is named by an environment variable. It is looked up in the process environment
//! first and then in `.env` files: the app's own (`<home>/.env`, written with mode 0600, where
//! keys entered in settings are stored) and, when the caller passes it, a project's `.env`.
//! A project `.env` that git tracks is ignored, so a committed key is never used silently.
//!
//! The format is the usual one: `NAME=value` per line, optional `export `, `#` comments,
//! optional single or double quotes around the value.

use std::io::Write;
use std::path::{Path, PathBuf};

/// File name of a secrets file, in the app home and in a project.
pub const ENV_FILE: &str = ".env";

/// Where a key was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The process environment.
    Environment,
    /// A `.env` file.
    File(PathBuf),
}

/// Whether `name` can be an environment variable name.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse `.env` text into `(name, value)` pairs, in file order. Malformed lines are skipped.
pub fn parse(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);
        let Some((name, raw)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !valid_name(name) {
            continue;
        }
        out.push((name.to_string(), unquote(raw.trim())));
    }
    out
}

fn unquote(raw: &str) -> String {
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        let mut out = String::new();
        let mut chars = raw[1..raw.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        raw[1..raw.len() - 1].to_string()
    } else {
        // Unquoted: an inline comment starts at " #".
        raw.split(" #").next().unwrap_or(raw).trim_end().to_string()
    }
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// True when `path` is a file git tracks (so its contents are, or will be, in history).
pub fn is_git_tracked(path: &Path) -> bool {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One `.env` file.
#[derive(Debug, Clone)]
pub struct EnvFile {
    path: PathBuf,
}

impl EnvFile {
    pub fn new(path: impl Into<PathBuf>) -> EnvFile {
        EnvFile { path: path.into() }
    }

    /// `<home>/.env`: the app's own secrets file.
    pub fn in_home(home: &Path) -> EnvFile {
        EnvFile::new(home.join(ENV_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> std::io::Result<String> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    /// The value of `name`; the last assignment wins. Empty values count as unset.
    pub fn get(&self, name: &str) -> std::io::Result<Option<String>> {
        Ok(parse(&self.read()?)
            .into_iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
            .filter(|v| !v.is_empty()))
    }

    /// Set `name`, replacing earlier assignments and keeping every other line. The file is
    /// created with mode 0600 and replaced atomically.
    pub fn set(&self, name: &str, value: &str) -> std::io::Result<()> {
        if !valid_name(name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("`{name}` is not a valid variable name"),
            ));
        }
        if value.contains(['\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a key cannot contain a line break",
            ));
        }
        let mut lines = self.without(name)?;
        lines.push(format!("{name}={}", quote(value)));
        self.write(&lines)
    }

    /// Remove every assignment of `name`.
    pub fn remove(&self, name: &str) -> std::io::Result<()> {
        let lines = self.without(name)?;
        self.write(&lines)
    }

    fn without(&self, name: &str) -> std::io::Result<Vec<String>> {
        Ok(self
            .read()?
            .lines()
            .filter(|l| !parse(l).iter().any(|(n, _)| n == name))
            .map(str::to_string)
            .collect())
    }

    fn write(&self, lines: &[String]) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        let tmp = self.path.with_extension("env.tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).write(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
    }
}

/// Look `name` up: the process environment first, then each file in order. A file that git
/// tracks is skipped. Read errors count as "not there".
pub fn lookup(name: &str, files: &[&Path]) -> Option<(String, Source)> {
    if let Some(v) = std::env::var(name).ok().filter(|v| !v.is_empty()) {
        return Some((v, Source::Environment));
    }
    for path in files {
        if is_git_tracked(path) {
            tracing::warn!(file = %path.display(), "ignoring a .env file that git tracks");
            continue;
        }
        if let Ok(Some(v)) = EnvFile::new(*path).get(name) {
            return Some((v, Source::File(path.to_path_buf())));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_usual_forms() {
        let text = "# a comment\n\nA=1\nexport B = two words \nC=\"quoted \\\"x\\\" # not a comment\"\nD='single $x'\nE=bare # comment\n1BAD=x\nno equals\nF=\n";
        assert_eq!(
            parse(text),
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two words".into()),
                ("C".into(), "quoted \"x\" # not a comment".into()),
                ("D".into(), "single $x".into()),
                ("E".into(), "bare".into()),
                ("F".into(), "".into()),
            ]
        );
    }

    #[test]
    fn set_get_remove_keep_other_lines_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let f = EnvFile::in_home(dir.path());
        assert_eq!(f.get("K").unwrap(), None);
        std::fs::write(f.path(), "# mine\nOTHER=keep\nK=old\n").unwrap();
        f.set("K", "s3cr\"et\\ value").unwrap();
        assert_eq!(f.get("K").unwrap().as_deref(), Some("s3cr\"et\\ value"));
        assert_eq!(f.get("OTHER").unwrap().as_deref(), Some("keep"));
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(text.starts_with("# mine\nOTHER=keep\n"), "{text}");
        assert_eq!(text.matches("K=").count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(f.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        f.remove("K").unwrap();
        assert_eq!(f.get("K").unwrap(), None);
        assert_eq!(f.get("OTHER").unwrap().as_deref(), Some("keep"));
        assert!(f.set("bad name", "x").is_err());
        assert!(f.set("K", "a\nb").is_err());
    }

    #[test]
    fn lookup_prefers_the_environment_and_skips_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let f = EnvFile::in_home(&home);
        f.set("WB_NET_TEST_ONLY_IN_FILE", "from-file").unwrap();
        f.set("PATH", "shadowed").unwrap();
        let files = [f.path()];
        assert_eq!(
            lookup("WB_NET_TEST_ONLY_IN_FILE", &files),
            Some(("from-file".into(), Source::File(f.path().to_path_buf())))
        );
        assert_eq!(lookup("PATH", &files).unwrap().1, Source::Environment);
        assert_eq!(lookup("WB_NET_TEST_MISSING", &files), None);

        // A project .env that is committed is not used.
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if git(&["init", "-q"]) {
            let project = EnvFile::new(repo.join(ENV_FILE));
            project.set("WB_NET_TEST_PROJECT_KEY", "k").unwrap();
            assert!(lookup("WB_NET_TEST_PROJECT_KEY", &[project.path()]).is_some());
            assert!(git(&["add", "-f", ".env"]));
            assert!(is_git_tracked(project.path()));
            assert_eq!(lookup("WB_NET_TEST_PROJECT_KEY", &[project.path()]), None);
        }
    }
}
