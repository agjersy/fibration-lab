//! Repository template (`analyst init`, `session_create { init: true }`): `.analyst/project.toml`,
//! `analysis/`, `scripts/`, `data/`, `out/`, `notes.md`, `pyproject.toml`, `.gitignore`, then
//! optionally `uv venv` + `uv sync` and `git init` with an initial commit.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const TEMPLATE_PROJECT_TOML: &str = r#"[project]
scripts = ["scripts/**/*.py"]
library = "analysis"
out_dir = "out"
data_dirs = ["data"]
timeout_s = 600

[contract]
max_helper_lines = 15
near_duplicate_threshold = 0.80
min_tokens_for_similarity = 30
math_required = []

[policy]
edits = "review"
runs = "auto"
packages = "ask"
shell = "ask"
network = "deny"
git = "ask"
protected = []
max_steps = 40
"#;

#[derive(Debug, Clone)]
pub struct InitOptions {
    /// Run `uv venv` and `uv sync` when `uv` is on PATH.
    pub run_uv: bool,
    /// `git init` plus an initial commit when the directory is not a repo yet.
    pub git: bool,
}

impl Default for InitOptions {
    fn default() -> Self {
        InitOptions {
            run_uv: true,
            git: true,
        }
    }
}

/// What `init_repo` did, for the caller to print or log.
#[derive(Debug, Clone, Default)]
pub struct InitReport {
    pub root: PathBuf,
    pub created: Vec<String>,
    pub kept: Vec<String>,
    /// Human-readable lines about the uv and git steps.
    pub log: Vec<String>,
}

/// The template files: `(relative path, content)`.
pub fn template_files(name: &str) -> Vec<(&'static str, String)> {
    vec![
        (".analyst/project.toml", TEMPLATE_PROJECT_TOML.into()),
        (
            "analysis/__init__.py",
            "\"\"\"Shared analysis library: documented, reusable functions.\"\"\"\n".into(),
        ),
        ("scripts/.gitkeep", String::new()),
        ("data/.gitkeep", String::new()),
        ("out/.gitkeep", String::new()),
        ("notes.md", format!("# {name}\n\nNotes and decisions.\n")),
        (
            ".gitignore",
            // `data/*` (not `data/`) so the `.gitkeep` exception can apply: data files are
            // never committed, checkpointed, or synced by default.
            "out/*\n!out/.gitkeep\ndata/*\n!data/.gitkeep\n.venv/\n__pycache__/\n*.pyc\n.analyst/env.sh\n.ruff_cache/\n"
                .into(),
        ),
        (
            "pyproject.toml",
            format!(
                "[project]\nname = \"{}\"\nversion = \"0.1.0\"\nrequires-python = \">=3.11\"\ndependencies = [\"numpy\", \"scipy\", \"matplotlib\", \"pandas\"]\n\n[tool.ruff]\nline-length = 100\n",
                name.replace(' ', "-").to_lowercase()
            ),
        ),
    ]
}

/// Whether `uv` can be found (PATH, the login shell's PATH, or a well-known location).
pub fn has_uv() -> bool {
    crate::util::find_tool("uv").is_some()
}

/// Apply the template to `root` (created if missing). Existing files are kept.
pub fn init_repo(root: &Path, opts: &InitOptions) -> Result<InitReport> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "analysis-repo".into());
    let mut report = InitReport {
        root: root.clone(),
        ..Default::default()
    };
    for (rel, content) in template_files(&name) {
        let p = root.join(rel);
        if p.exists() {
            report.kept.push(rel.to_string());
            continue;
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&p, content).with_context(|| format!("writing {}", p.display()))?;
        report.created.push(rel.to_string());
    }
    if opts.run_uv {
        if has_uv() {
            for args in [vec!["venv", "-q"], vec!["sync", "-q"]] {
                let st = std::process::Command::new(crate::util::resolve_tool("uv"))
                    .env("PATH", crate::util::tool_path_env())
                    .args(&args)
                    .current_dir(&root)
                    .stdin(std::process::Stdio::null())
                    .status()
                    .with_context(|| format!("running uv {}", args.join(" ")))?;
                report.log.push(format!(
                    "uv {} -> {}",
                    args.join(" "),
                    if st.success() { "ok" } else { "failed" }
                ));
            }
        } else {
            report
                .log
                .push("uv not found on PATH; skipped `uv venv` and `uv sync`".into());
        }
    }
    if opts.git && !root.join(".git").exists() {
        crate::checkpoint::git(&root, &["init", "-q"], None)?;
        crate::checkpoint::git(&root, &["add", "-A"], None)?;
        crate::checkpoint::git(&root, &["commit", "-q", "-m", "analyst init"], None)?;
        report.log.push("git init + initial commit".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_idempotent_and_committed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("my repo");
        let r = init_repo(
            &root,
            &InitOptions {
                run_uv: false,
                git: true,
            },
        )
        .unwrap();
        assert!(r.created.iter().any(|c| c == ".analyst/project.toml"));
        assert!(root.join("analysis/__init__.py").is_file());
        assert!(root.join(".git").is_dir());
        let py = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(py.contains("name = \"my-repo\""));
        let again = init_repo(
            &root,
            &InitOptions {
                run_uv: false,
                git: true,
            },
        )
        .unwrap();
        assert!(again.created.is_empty());
        assert_eq!(again.kept.len(), template_files("x").len());
        let cfg = crate::config::load_project(&root).unwrap();
        assert_eq!(cfg, analyst_protocol::config::ProjectConfig::default());
        // A8: data files are ignored, the placeholders are tracked.
        std::fs::write(root.join("data/big.csv"), "1,2\n").unwrap();
        std::fs::write(root.join("out/result.txt"), "r\n").unwrap();
        let st = crate::checkpoint::git(&root, &["status", "--porcelain"], None).unwrap();
        assert_eq!(st.trim(), "", "data and out files must be ignored: {st}");
        let tracked = crate::checkpoint::git(&root, &["ls-files"], None).unwrap();
        assert!(tracked.contains("data/.gitkeep") && tracked.contains("out/.gitkeep"));
        assert!(!tracked.contains("big.csv"));
    }
}
