//! Refuse projects that name their own package sources (ARCHITECTURE 5.6).
//!
//! An install is authorized for one destination: the configured `[packages] index_url`. A
//! project can send `uv` elsewhere without any command-line option, through its own files:
//! extra indexes, `tool.uv.sources`, direct URL or VCS requirements, or a lockfile that
//! pins packages to another registry. [`check`] reads those files before an install and
//! fails, naming the entry and the file, when one of them points anywhere else.
//!
//! Files read, relative to the project root: `pyproject.toml` (and the `pyproject.toml` of
//! every workspace member and in-repo path source), `uv.toml`, `uv.lock`, and
//! `requirements.txt` (which the remote session setup installs when there is no
//! `pyproject.toml`).

use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use toml::Value;

/// One refused entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// File the entry is in, relative to the project root.
    pub file: String,
    /// The entry, as the user would find it in the file.
    pub entry: String,
}

/// Fail when the project in `root` names a package source other than `index`.
pub fn check(root: &Path, index: &str) -> Result<()> {
    match first_violation(root, index)? {
        None => Ok(()),
        Some(v) => Err(anyhow!(
            "not installing: {} names its own package source ({}). The app installs only from the configured package index ({index}). Remove that entry, or set `[packages] index_url` in config.toml if that index is the one to use",
            v.file,
            v.entry
        )),
    }
}

/// The first entry [`check`] would refuse. An unreadable or unparsable project file is an
/// error: what cannot be read cannot be cleared.
pub fn first_violation(root: &Path, index: &str) -> Result<Option<Violation>> {
    // Absolute, so "inside the repository" can be decided by comparing path prefixes.
    let root = &lexical(&std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()));
    let mut cx = Cx {
        root: root.clone(),
        index: normalize_index(index),
        seen: HashSet::new(),
    };
    if let Some(v) = cx.pyproject(root)? {
        return Ok(Some(v));
    }
    if let Some(doc) = cx.read_toml(&root.join("uv.toml"))? {
        if let Some(v) = cx.settings(&doc, "", "uv.toml") {
            return Ok(Some(v));
        }
    }
    if let Some(doc) = cx.read_toml(&root.join("uv.lock"))? {
        if let Some(v) = cx.lock(&doc) {
            return Ok(Some(v));
        }
    }
    cx.requirements(&root.join("requirements.txt"), 0)
}

struct Cx {
    root: PathBuf,
    index: String,
    seen: HashSet<PathBuf>,
}

/// Scheme and host lowercased, surrounding whitespace and trailing slashes dropped.
fn normalize_index(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let sep = if path.is_empty() { "" } else { "/" };
            format!(
                "{}://{}{sep}{path}",
                scheme.to_ascii_lowercase(),
                host.to_ascii_lowercase()
            )
        }
        None => url.to_string(),
    }
}

/// `path` with `.` and `..` resolved without touching the file system.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether a requirement string is a direct reference (`name @ <url>`) or a bare URL or VCS
/// reference, as opposed to a name with version specifiers.
pub fn is_direct_reference(requirement: &str) -> bool {
    let r = requirement.trim();
    let lower = r.to_ascii_lowercase();
    const SCHEMES: &[&str] = &[
        "git+", "hg+", "svn+", "bzr+", "http://", "https://", "ftp://", "file:", "git://", "ssh://",
    ];
    if SCHEMES.iter().any(|s| lower.starts_with(s)) {
        return true;
    }
    // PEP 508: `name [extras] @ url`.
    let rest = r.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || "._-".contains(c));
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix('[') {
        Some(inner) => inner.split_once(']').map(|(_, r)| r).unwrap_or(""),
        None => rest,
    };
    rest.trim_start().starts_with('@')
}

fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn violation(file: &str, entry: String) -> Option<Violation> {
    Some(Violation {
        file: file.to_string(),
        entry,
    })
}

impl Cx {
    fn rel(&self, path: &Path) -> String {
        lexical(path)
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.display().to_string())
    }

    fn inside(&self, path: &Path) -> bool {
        lexical(path).starts_with(&self.root)
    }

    fn read_toml(&self, path: &Path) -> Result<Option<Value>> {
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("not installing: cannot read {} ({e})", self.rel(path)))?;
        let doc: Value = toml::from_str(&text).map_err(|e| {
            anyhow!(
                "not installing: {} is not valid TOML, so its package sources cannot be checked ({})",
                self.rel(path),
                e.message()
            )
        })?;
        Ok(Some(doc))
    }

    fn other_index(&self, url: &str) -> bool {
        normalize_index(url) != self.index
    }

    /// Index settings shared by `[tool.uv]` in `pyproject.toml` and the top level of
    /// `uv.toml`: `index`, `index-url`, `extra-index-url`, `find-links`, and the same under
    /// `pip`. `prefix` is `tool.uv.` or empty.
    fn settings(&self, table: &Value, prefix: &str, file: &str) -> Option<Violation> {
        for (scope, t) in [("", Some(table)), ("pip.", table.get("pip"))] {
            let Some(t) = t else { continue };
            for entry in t
                .get("index")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let url = entry.get("url").and_then(Value::as_str).unwrap_or("");
                if self.other_index(url) {
                    let name = entry
                        .get("name")
                        .and_then(Value::as_str)
                        .map(|n| format!("name = \"{n}\", "))
                        .unwrap_or_default();
                    return violation(
                        file,
                        format!("[[{prefix}{scope}index]] {name}url = \"{url}\""),
                    );
                }
            }
            for key in ["index-url", "extra-index-url", "find-links"] {
                for url in t.get(key).map(strings).unwrap_or_default() {
                    if self.other_index(url) {
                        return violation(file, format!("{prefix}{scope}{key} = \"{url}\""));
                    }
                }
            }
        }
        None
    }

    /// `pyproject.toml` in `dir`, then those of its workspace members and in-repo path
    /// sources.
    fn pyproject(&mut self, dir: &Path) -> Result<Option<Violation>> {
        let path = dir.join("pyproject.toml");
        if !self.seen.insert(lexical(&path)) {
            return Ok(None);
        }
        let Some(doc) = self.read_toml(&path)? else {
            return Ok(None);
        };
        let file = self.rel(&path);
        let mut next: Vec<PathBuf> = Vec::new();
        let uv = doc.get("tool").and_then(|t| t.get("uv"));
        if let Some(uv) = uv {
            if let Some(v) = self.settings(uv, "tool.uv.", &file) {
                return Ok(Some(v));
            }
            for (name, source) in uv
                .get("sources")
                .and_then(Value::as_table)
                .into_iter()
                .flatten()
            {
                let entries: Vec<&Value> = match source {
                    Value::Array(a) => a.iter().collect(),
                    other => vec![other],
                };
                for s in entries {
                    for key in ["git", "url"] {
                        if let Some(target) = s.get(key).and_then(Value::as_str) {
                            return Ok(violation(
                                &file,
                                format!("tool.uv.sources.{name}: {key} = \"{target}\""),
                            ));
                        }
                    }
                    if let Some(p) = s.get("path").and_then(Value::as_str) {
                        let target = dir.join(p);
                        if !self.inside(&target) {
                            return Ok(violation(
                                &file,
                                format!(
                                    "tool.uv.sources.{name}: path = \"{p}\", which is outside the repository"
                                ),
                            ));
                        }
                        next.push(target);
                    }
                }
            }
            for key in [
                "dev-dependencies",
                "constraint-dependencies",
                "override-dependencies",
                "build-constraint-dependencies",
            ] {
                for req in uv.get(key).map(strings).unwrap_or_default() {
                    if is_direct_reference(req) {
                        return Ok(violation(&file, format!("tool.uv.{key}: \"{req}\"")));
                    }
                }
            }
            let members = uv
                .get("workspace")
                .and_then(|w| w.get("members"))
                .map(strings)
                .unwrap_or_default();
            for pattern in members {
                let full = dir.join(pattern);
                for hit in glob::glob(&full.to_string_lossy())
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    if self.inside(&hit) {
                        next.push(hit);
                    }
                }
            }
        }
        let mut lists: Vec<(String, Vec<&str>)> = Vec::new();
        if let Some(project) = doc.get("project") {
            if let Some(d) = project.get("dependencies") {
                lists.push(("project.dependencies".into(), strings(d)));
            }
            for (extra, d) in project
                .get("optional-dependencies")
                .and_then(Value::as_table)
                .into_iter()
                .flatten()
            {
                lists.push((format!("project.optional-dependencies.{extra}"), strings(d)));
            }
        }
        for (group, d) in doc
            .get("dependency-groups")
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
        {
            lists.push((format!("dependency-groups.{group}"), strings(d)));
        }
        if let Some(d) = doc.get("build-system").and_then(|b| b.get("requires")) {
            lists.push(("build-system.requires".into(), strings(d)));
        }
        for (key, reqs) in lists {
            if let Some(req) = reqs.into_iter().find(|r| is_direct_reference(r)) {
                return Ok(violation(&file, format!("{key}: \"{req}\"")));
            }
        }
        for dir in next {
            if dir.is_dir() {
                if let Some(v) = self.pyproject(&dir)? {
                    return Ok(Some(v));
                }
            }
        }
        Ok(None)
    }

    /// `uv.lock`: every package's `source` is the configured registry or a local path.
    fn lock(&self, doc: &Value) -> Option<Violation> {
        for package in doc
            .get("package")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = package.get("name").and_then(Value::as_str).unwrap_or("?");
            let Some(source) = package.get("source") else {
                continue;
            };
            for key in ["git", "url"] {
                if let Some(target) = source.get(key).and_then(Value::as_str) {
                    return violation(
                        "uv.lock",
                        format!("package `{name}`: source {key} = \"{target}\""),
                    );
                }
            }
            if let Some(registry) = source.get("registry").and_then(Value::as_str) {
                if self.other_index(registry) {
                    return violation(
                        "uv.lock",
                        format!("package `{name}`: source registry = \"{registry}\""),
                    );
                }
            }
        }
        None
    }

    /// `requirements.txt`: index options, URL and VCS requirements, and included files.
    fn requirements(&mut self, path: &Path, depth: usize) -> Result<Option<Violation>> {
        if !path.is_file() || !self.seen.insert(lexical(path)) {
            return Ok(None);
        }
        let file = self.rel(path);
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("not installing: cannot read {file} ({e})"))?;
        for raw in text.lines() {
            let line = raw.split(" #").next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (opt, value) = match line.split_once([' ', '=']) {
                Some((o, v)) if o.starts_with('-') => (o, v.trim()),
                _ if line.starts_with('-') => (line, ""),
                _ => ("", line),
            };
            match opt {
                "-i" | "--index-url" | "--extra-index-url" | "-f" | "--find-links" => {
                    if self.other_index(value) {
                        return Ok(violation(&file, line.to_string()));
                    }
                }
                "-r" | "--requirement" | "-c" | "--constraint" => {
                    let included = path.parent().unwrap_or(Path::new(".")).join(value);
                    if is_direct_reference(value) || !self.inside(&included) || depth >= 8 {
                        return Ok(violation(&file, line.to_string()));
                    }
                    if let Some(v) = self.requirements(&included, depth + 1)? {
                        return Ok(Some(v));
                    }
                }
                _ => {
                    if is_direct_reference(value) {
                        return Ok(violation(&file, line.to_string()));
                    }
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "https://pypi.org/simple";

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    /// The refusal for a project made of `files`: `(file, entry)`.
    fn refused(files: &[(&str, &str)]) -> (String, String) {
        let dir = project(files);
        let v = first_violation(dir.path(), INDEX)
            .unwrap()
            .unwrap_or_else(|| panic!("{files:?} was not refused"));
        let err = check(dir.path(), INDEX).unwrap_err().to_string();
        assert!(err.contains(&v.file) && err.contains(&v.entry), "{err}");
        assert!(err.contains(INDEX) && err.contains("index_url"), "{err}");
        (v.file, v.entry)
    }

    const CLEAN_PYPROJECT: &str = r#"
[project]
name = "demo"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = ["numpy>=2", "matplotlib[extra] ; python_version > '3.8'", "local-lib"]

[project.optional-dependencies]
fit = ["scipy"]

[dependency-groups]
dev = ["pytest>=8", { include-group = "lint" }]
lint = ["ruff"]

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[[tool.uv.index]]
name = "main"
url = "https://PyPI.org/simple/"

[tool.uv]
index-url = "https://pypi.org/simple"

[tool.uv.sources]
local-lib = { path = "libs/local-lib", editable = true }
numpy = { index = "main" }
member = { workspace = true }

[tool.uv.workspace]
members = ["packages/*"]
"#;

    const CLEAN_LOCK: &str = r#"
version = 1
requires-python = ">=3.11"

[[package]]
name = "demo"
version = "0.1.0"
source = { virtual = "." }
dependencies = [{ name = "numpy" }]

[[package]]
name = "local-lib"
version = "0.1.0"
source = { editable = "libs/local-lib" }

[[package]]
name = "numpy"
version = "2.1.0"
source = { registry = "https://pypi.org/simple" }
wheels = [{ url = "https://files.pythonhosted.org/packages/numpy-2.1.0-py3-none-any.whl", hash = "sha256:00" }]
"#;

    #[test]
    fn a_clean_project_passes() {
        let dir = project(&[
            ("pyproject.toml", CLEAN_PYPROJECT),
            ("uv.lock", CLEAN_LOCK),
            (
                "libs/local-lib/pyproject.toml",
                "[project]\nname = \"local-lib\"\nversion = \"0.1.0\"\ndependencies = [\"attrs\"]\n",
            ),
            (
                "packages/member/pyproject.toml",
                "[project]\nname = \"member\"\nversion = \"0.1.0\"\n",
            ),
            ("uv.toml", "index-url = \"https://pypi.org/simple/\"\n"),
            (
                "requirements.txt",
                "# pinned\nnumpy==2.1.0  # comment\n--index-url https://pypi.org/simple\n-r more.txt\n-e ./libs/local-lib\n",
            ),
            ("more.txt", "scipy>=1.10\n"),
        ]);
        assert_eq!(first_violation(dir.path(), INDEX).unwrap(), None);
        check(dir.path(), INDEX).unwrap();
        // No project files at all is clean too.
        check(tempfile::tempdir().unwrap().path(), INDEX).unwrap();
        // The same project is refused when another index is the configured one.
        let err = check(dir.path(), "https://mirror.corp.example/simple")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("PyPI.org") && err.contains("pyproject.toml"),
            "{err}"
        );
    }

    #[test]
    fn index_settings_that_point_elsewhere_are_refused() {
        let (file, entry) = refused(&[(
            "pyproject.toml",
            "[[tool.uv.index]]\nname = \"corp\"\nurl = \"https://packages.elsewhere.example/simple\"\n",
        )]);
        assert_eq!(file, "pyproject.toml");
        assert_eq!(
            entry,
            "[[tool.uv.index]] name = \"corp\", url = \"https://packages.elsewhere.example/simple\""
        );
        for (key, value) in [
            ("index-url", "\"https://elsewhere.example/simple\""),
            ("extra-index-url", "[\"https://elsewhere.example/simple\"]"),
            ("find-links", "[\"https://elsewhere.example/wheels\"]"),
            ("find-links", "[\"./wheels\"]"),
        ] {
            let text = format!("[tool.uv]\n{key} = {value}\n");
            let (file, entry) = refused(&[("pyproject.toml", &text)]);
            assert_eq!(file, "pyproject.toml");
            assert!(entry.starts_with(&format!("tool.uv.{key} = ")), "{entry}");
            // The same setting for `uv pip`, and in a project-level uv.toml.
            let text = format!("[tool.uv.pip]\n{key} = {value}\n");
            let (_, entry) = refused(&[("pyproject.toml", &text)]);
            assert!(
                entry.starts_with(&format!("tool.uv.pip.{key} = ")),
                "{entry}"
            );
            let text = format!("{key} = {value}\n");
            let (file, entry) = refused(&[("uv.toml", &text)]);
            assert_eq!(file, "uv.toml");
            assert!(entry.starts_with(&format!("{key} = ")), "{entry}");
        }
        let (file, _) = refused(&[(
            "uv.toml",
            "[[index]]\nurl = \"https://elsewhere.example/simple\"\ndefault = true\n",
        )]);
        assert_eq!(file, "uv.toml");
    }

    #[test]
    fn git_and_url_sources_are_refused_and_repo_paths_are_not() {
        let (_, entry) = refused(&[(
            "pyproject.toml",
            "[tool.uv.sources]\nhelper = { git = \"https://github.com/someone/helper\", rev = \"abc\" }\n",
        )]);
        assert_eq!(
            entry,
            "tool.uv.sources.helper: git = \"https://github.com/someone/helper\""
        );
        let (_, entry) = refused(&[(
            "pyproject.toml",
            "[tool.uv.sources]\nhelper = [{ url = \"https://example.org/helper-1.0.tar.gz\", marker = \"sys_platform == 'linux'\" }]\n",
        )]);
        assert!(
            entry.starts_with("tool.uv.sources.helper: url = "),
            "{entry}"
        );
        let (_, entry) = refused(&[(
            "pyproject.toml",
            "[tool.uv.sources]\nhelper = { path = \"../outside\" }\n",
        )]);
        assert!(entry.contains("outside the repository"), "{entry}");
        // An in-repo path source is allowed, and its own pyproject is read.
        let outer = "[tool.uv.sources]\nhelper = { path = \"libs/helper\" }\n";
        check(project(&[("pyproject.toml", outer)]).path(), INDEX).unwrap();
        let (file, entry) = refused(&[
            ("pyproject.toml", outer),
            (
                "libs/helper/pyproject.toml",
                "[tool.uv.sources]\ndeep = { git = \"ssh://git@example.org/deep.git\" }\n",
            ),
        ]);
        assert_eq!(file, "libs/helper/pyproject.toml");
        assert!(entry.starts_with("tool.uv.sources.deep: git = "), "{entry}");
        // Workspace members are read as well.
        let (file, _) = refused(&[
            ("pyproject.toml", "[tool.uv.workspace]\nmembers = [\"packages/*\"]\n"),
            (
                "packages/a/pyproject.toml",
                "[project]\nname = \"a\"\nversion = \"0\"\ndependencies = [\"x @ https://example.org/x.whl\"]\n",
            ),
        ]);
        assert_eq!(file, "packages/a/pyproject.toml");
    }

    #[test]
    fn direct_url_and_vcs_requirements_are_refused() {
        for req in [
            "helper @ https://example.org/helper-1.0-py3-none-any.whl",
            "helper@git+https://github.com/someone/helper@main",
            "helper[fast] @ git+ssh://git@github.com/someone/helper",
            "git+https://github.com/someone/helper",
            "https://example.org/helper-1.0.tar.gz",
            "helper @ file:///tmp/helper",
        ] {
            assert!(is_direct_reference(req), "{req}");
            for (table, key) in [
                ("project", "dependencies"),
                ("project.optional-dependencies", "extra"),
                ("dependency-groups", "dev"),
                ("build-system", "requires"),
                ("tool.uv", "dev-dependencies"),
            ] {
                let text = format!("[{table}]\n{key} = [\"numpy\", \"{req}\"]\n");
                let (file, entry) = refused(&[("pyproject.toml", &text)]);
                assert_eq!(file, "pyproject.toml");
                assert!(entry.contains(req) && entry.contains(key), "{entry}");
            }
            let (file, entry) = refused(&[("requirements.txt", &format!("numpy\n{req}\n"))]);
            assert_eq!((file.as_str(), entry.as_str()), ("requirements.txt", req));
        }
        for req in [
            "numpy",
            "numpy>=2,<3",
            "matplotlib[svg]==3.9 ; python_version >= '3.11'",
            "my.pkg_name-x ~= 1.0",
            "email-validator; extra == 'email'",
        ] {
            assert!(!is_direct_reference(req), "{req}");
        }
        for line in [
            "--extra-index-url https://elsewhere.example/simple",
            "--index-url=https://elsewhere.example/simple",
            "-f https://elsewhere.example/wheels",
            "-e git+https://github.com/someone/helper#egg=helper",
            "-r ../outside.txt",
        ] {
            let (_, entry) = refused(&[("requirements.txt", line)]);
            assert_eq!(entry, line);
        }
        let (file, _) = refused(&[
            ("requirements.txt", "-r sub/more.txt\n"),
            ("sub/more.txt", "x @ https://example.org/x.whl\n"),
        ]);
        assert_eq!(file, "sub/more.txt");
    }

    #[test]
    fn lockfile_sources_other_than_the_index_are_refused() {
        let lock = |source: &str| {
            format!(
                "version = 1\n\n[[package]]\nname = \"numpy\"\nversion = \"2\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\n\n[[package]]\nname = \"helper\"\nversion = \"1\"\nsource = {source}\n"
            )
        };
        for (source, expect) in [
            (
                "{ git = \"https://github.com/someone/helper?rev=abc#abc\" }",
                "package `helper`: source git = \"https://github.com/someone/helper?rev=abc#abc\"",
            ),
            (
                "{ url = \"https://example.org/helper-1.0.tar.gz\" }",
                "package `helper`: source url = \"https://example.org/helper-1.0.tar.gz\"",
            ),
            (
                "{ registry = \"https://packages.elsewhere.example/simple\" }",
                "package `helper`: source registry = \"https://packages.elsewhere.example/simple\"",
            ),
            (
                "{ registry = \"../wheels\" }",
                "package `helper`: source registry = \"../wheels\"",
            ),
        ] {
            let (file, entry) = refused(&[("uv.lock", &lock(source))]);
            assert_eq!((file.as_str(), entry.as_str()), ("uv.lock", expect));
        }
        for source in [
            "{ path = \"wheels/helper.whl\" }",
            "{ directory = \"libs/helper\" }",
        ] {
            check(project(&[("uv.lock", &lock(source))]).path(), INDEX).unwrap();
        }
    }

    #[test]
    fn unreadable_project_files_are_refused() {
        let dir = project(&[("pyproject.toml", "[tool.uv\nindex-url = ")]);
        let err = check(dir.path(), INDEX).unwrap_err().to_string();
        assert!(
            err.contains("pyproject.toml") && err.contains("not valid TOML"),
            "{err}"
        );
    }
}
