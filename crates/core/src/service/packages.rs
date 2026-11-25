//! The package service: `uv add | remove | sync` for the session environment, the one
//! implementation behind the UI's `env_add` / `env_remove` / `env_sync` tasks and the agent's
//! `pkg.add` / `pkg.remove` / `pkg.sync` tools (DESIGN-REVIEW item 1).
//!
//! The local repo is canonical, also for the environment: on a remote target the change is
//! made to the local `pyproject.toml` and `uv.lock` first, then the mirror is synced, then the
//! host runs `uv sync` (R5). Changing only the mirror would be undone by the next push.
//!
//! Everything that resolves or downloads packages is built by [`crate::egress::uv_install`]
//! and so names the configured index; an uninstall needs no index and runs offline.

use super::runs::RunService;
use super::tasks::TaskLogger;
use crate::target::{self, Target};
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

/// A package operation: `add`, `remove`, or `sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Remove,
    Sync,
}

impl Op {
    /// From a UI operation (`add`) or an agent tool id (`pkg.add`).
    pub fn parse(s: &str) -> Result<Op> {
        match s.strip_prefix("pkg.").unwrap_or(s) {
            "add" => Ok(Op::Add),
            "remove" => Ok(Op::Remove),
            "sync" => Ok(Op::Sync),
            other => bail!("unknown package operation `{other}`"),
        }
    }
}

/// Package arguments are requirement specifiers, never options (A9).
pub fn validate_names(packages: &[String]) -> Result<()> {
    for p in packages {
        if p.trim().is_empty() {
            bail!("empty package name");
        }
        if p.trim_start().starts_with('-') {
            bail!(
                "`{p}` is not a package name (it starts with `-` and would be read as an option)"
            );
        }
        if p.chars().any(|c| c.is_control()) {
            bail!(
                "package name `{}` contains control characters",
                p.escape_debug()
            );
        }
    }
    Ok(())
}

/// What the egress guard does for an install line when it knows the directory, done here
/// because task commands are spawned without telling it: refuse a project that names its own
/// package sources, and report a missing interpreter with the version to install instead of
/// starting `uv`.
pub async fn check_install(repo: &Path, line: &[String]) -> Result<()> {
    if !line.iter().any(|a| a == crate::egress::INDEX_FLAG) {
        return Ok(());
    }
    let repo = repo.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::egress::check_project_sources(&repo)?;
        crate::egress::require_python(&repo, None)
    })
    .await
    .map_err(|e| anyhow!("install check failed: {e}"))?
}

/// The `uv` command line run in the local repo. On a remote target with a pyproject, adds
/// and removals only update `pyproject.toml` and `uv.lock` (`--no-sync`) and a sync only
/// re-locks. `python` names the session interpreter for a repo without a pyproject.
pub fn local_line(
    uv: &str,
    op: Op,
    has_pyproject: bool,
    remote_target: bool,
    packages: &[String],
    python: Option<&str>,
) -> Result<Vec<String>> {
    let mut rest: Vec<String> = Vec::new();
    if !has_pyproject {
        if let Some(p) = python {
            rest.extend(["--python".to_string(), p.to_string()]);
        }
    }
    if has_pyproject && remote_target && op != Op::Sync {
        rest.push("--no-sync".into());
    }
    if op != Op::Sync {
        rest.push("--".into());
        rest.extend(packages.iter().cloned());
    }
    let install = |sub: &[&str]| crate::egress::uv_install(uv, sub, &rest);
    match (op, has_pyproject) {
        (Op::Add, true) => Ok(install(&["add"])),
        (Op::Remove, true) => Ok(install(&["remove"])),
        (Op::Add, false) => Ok(install(&["pip", "install"])),
        (Op::Remove, false) => {
            let mut line: Vec<String> = vec![uv.to_string(), "pip".into(), "uninstall".into()];
            line.extend(rest.iter().cloned());
            Ok(line)
        }
        // Resolving needs the index even when nothing is installed here.
        (Op::Sync, true) if remote_target => Ok(install(&["lock"])),
        (Op::Sync, true) => Ok(install(&["sync"])),
        (Op::Sync, false) => bail!("pkg.sync needs a pyproject.toml / uv.lock"),
    }
}

/// The remote shell command for a repo without a pyproject (there is no project file to keep
/// canonical, so the remote venv is changed directly). `index_flag` is the authorized
/// `--default-index '<url>'` from [`crate::egress::remote_index_flag`]; a removal needs no
/// index and runs offline.
pub fn remote_pip_command(op: Op, mirror: &str, packages: &[String], index_flag: &str) -> String {
    let pkgs: Vec<String> = packages.iter().map(|p| crate::util::sh_quote(p)).collect();
    let uv_call = if op == Op::Remove {
        "UV_OFFLINE=1 ~/.fibration/bin/uv pip uninstall".to_string()
    } else {
        format!("~/.fibration/bin/uv pip install {index_flag}")
    };
    format!(
        "cd {} && {uv_call} --python .venv/bin/python -- {}",
        crate::util::sh_quote_path(mirror),
        pkgs.join(" ")
    )
}

/// One package change.
pub struct Request<'a> {
    pub op: Op,
    pub packages: Vec<String>,
    pub repo: &'a Path,
    pub target: Target,
    pub control_dir: Option<PathBuf>,
    /// The session interpreter, for a repo without a pyproject.
    pub session_python: Option<String>,
}

async fn exec(
    line: &[String],
    cwd: Option<&Path>,
    logger: Option<&TaskLogger>,
    log: &mut String,
) -> Result<()> {
    match logger {
        Some(l) => l.stream_command(line, cwd).await,
        None => {
            let out = target::run_command(line, cwd).await?;
            log.push_str(&String::from_utf8_lossy(&out.stdout));
            log.push_str(&String::from_utf8_lossy(&out.stderr));
            Ok(())
        }
    }
}

/// Make the change. Output is streamed to `logger` when there is one (a UI task), and
/// returned otherwise (an agent tool result). `runs` syncs the mirror of a remote target.
pub async fn change(
    req: &Request<'_>,
    runs: &RunService,
    logger: Option<&TaskLogger>,
) -> Result<String> {
    if req.op != Op::Sync && req.packages.is_empty() {
        bail!("`packages` must list at least one package");
    }
    validate_names(&req.packages)?;
    let has_pyproject = req.repo.join("pyproject.toml").is_file();
    let uv = crate::util::tool("uv");
    let mut log = String::new();
    let note = |m: &str| {
        if let Some(l) = logger {
            l.log(m);
        }
    };
    match &req.target {
        Target::Local => {
            let line = local_line(
                &uv,
                req.op,
                has_pyproject,
                false,
                &req.packages,
                req.session_python.as_deref(),
            )?;
            check_install(req.repo, &line).await?;
            exec(&line, Some(req.repo), logger, &mut log).await?;
        }
        Target::Ssh { host, .. } => {
            let control = req.control_dir.as_deref();
            let mirror = req.target.mirror_dir(req.repo).unwrap_or_default();
            if has_pyproject {
                // 1. Change the canonical project files locally (no local venv).
                let line = local_line(&uv, req.op, true, true, &req.packages, None)?;
                check_install(req.repo, &line).await?;
                exec(&line, Some(req.repo), logger, &mut log).await?;
                // 2. Push them, 3. bring the remote environment in line.
                note("syncing the repository mirror");
                runs.mark_dirty();
                runs.sync_if_needed()
                    .await
                    .map_err(|e| anyhow!("sync after the package change failed: {}", e.message))?;
                let index_flag = crate::egress::remote_index_flag("uv sync (remote)")?;
                let cmd = target::bootstrap::session_env_command(&mirror, &index_flag);
                exec(
                    &target::ssh_command(host, control, &cmd),
                    None,
                    logger,
                    &mut log,
                )
                .await?;
            } else {
                runs.sync_if_needed()
                    .await
                    .map_err(|e| anyhow!("sync before the package change failed: {}", e.message))?;
                let index_flag = if req.op == Op::Add {
                    crate::egress::remote_index_flag("uv pip install (remote)")?
                } else {
                    String::new()
                };
                let remote = remote_pip_command(req.op, &mirror, &req.packages, &index_flag);
                exec(
                    &target::ssh_command(host, control, &remote),
                    None,
                    logger,
                    &mut log,
                )
                .await?;
            }
        }
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_command_construction() {
        for bad in ["--index-url=http://evil", "-e", ""] {
            let e = validate_names(&[bad.to_string()]).unwrap_err();
            assert!(e.to_string().contains("package name"), "{e}");
        }
        let pk = vec!["numpy>=2".to_string(), "pandas".to_string()];
        // Installs name the configured index right after the subcommand; the gate
        // authorizes exactly that index when the line is spawned.
        let index = crate::egress::package_index();
        let ix = index.as_str();
        assert_eq!(
            local_line("uv", Op::Add, true, false, &pk, None).unwrap(),
            vec![
                "uv",
                "add",
                "--default-index",
                ix,
                "--",
                "numpy>=2",
                "pandas"
            ]
        );
        assert_eq!(
            local_line("uv", Op::Add, true, true, &pk, None).unwrap(),
            vec![
                "uv",
                "add",
                "--default-index",
                ix,
                "--no-sync",
                "--",
                "numpy>=2",
                "pandas"
            ]
        );
        assert_eq!(
            local_line("uv", Op::Add, false, false, &pk, Some(".venv/bin/python")).unwrap(),
            vec![
                "uv",
                "pip",
                "install",
                "--default-index",
                ix,
                "--python",
                ".venv/bin/python",
                "--",
                "numpy>=2",
                "pandas"
            ]
        );
        // An uninstall needs no index: no flag, so it runs offline.
        let uninstall = local_line("uv", Op::Remove, false, false, &pk, None).unwrap();
        assert_eq!(
            uninstall,
            vec!["uv", "pip", "uninstall", "--", "numpy>=2", "pandas"]
        );
        assert!(crate::egress::guard(&uninstall)
            .unwrap()
            .env
            .contains(&("UV_OFFLINE".to_string(), "1".to_string())));
        assert_eq!(
            local_line("uv", Op::Sync, true, false, &[], None).unwrap(),
            vec!["uv", "sync", "--default-index", ix]
        );
        // On a remote target a sync re-locks locally; the host syncs to the pushed lockfile.
        assert_eq!(
            local_line("uv", Op::Sync, true, true, &[], None).unwrap(),
            vec!["uv", "lock", "--default-index", ix]
        );
        assert!(local_line("uv", Op::Sync, false, false, &[], None).is_err());
        let flag = "--default-index 'https://pypi.org/simple'";
        assert_eq!(
            remote_pip_command(Op::Add, "~/fibration/my repo-1a2b3c4d", &pk, flag),
            "cd ~/'fibration/my repo-1a2b3c4d' && ~/.fibration/bin/uv pip install --default-index 'https://pypi.org/simple' --python .venv/bin/python -- 'numpy>=2' pandas"
        );
        assert_eq!(
            remote_pip_command(Op::Remove, "~/fibration/r", &pk, flag),
            "cd ~/fibration/r && UV_OFFLINE=1 ~/.fibration/bin/uv pip uninstall --python .venv/bin/python -- 'numpy>=2' pandas"
        );
        assert_eq!(Op::parse("pkg.remove").unwrap(), Op::Remove);
        assert!(Op::parse("upgrade").is_err());
    }
}
