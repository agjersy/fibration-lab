//! Execution targets (ARCHITECTURE 5.5, PROTOCOL 7 and 9): the `Target` enum, local and
//! remote bootstrap, and repo sync (a git push of the working tree to a bare repository on
//! the host, a detached checkout as the mirror, and the pull of rendered artifacts).

use crate::util::{sh_quote, sh_quote_path};
use analyst_protocol::config::{TargetConfig, TargetKind};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Local,
    Ssh {
        name: String,
        host: String,
        remote_root: String,
        python: String,
        sandbox: String,
        data_dirs: Vec<String>,
        /// A local `uv` binary for this host's platform (`uv_binary` in the target config),
        /// copied over when the host has no `uv` and the local one does not match.
        uv_binary: Option<String>,
    },
}

/// How runs are started on a target (ARCHITECTURE 5.5, decision D17).
///
/// This is the extension point for scheduler integration. Only [`Launcher::Direct`] exists
/// today: the kernel starts each run as a detached process on the target (on a cluster, the
/// login node). A Slurm launcher would be selected per target here and implemented in the
/// kernel behind the same run protocol (`submit`, `status`, `cancel`, `output_paths`), so
/// nothing above the target layer changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Launcher {
    /// Start the script directly on the target. The only implemented launcher.
    #[default]
    Direct,
    /// NOT IMPLEMENTED. Placeholder for submitting runs through a scheduler such as Slurm.
    /// No configuration selects it and no code path constructs it.
    Scheduler,
}

/// Reject host strings that ssh, scp, or git would parse as something else: empty, starting
/// with `-` (an option), or containing whitespace or control characters.
pub fn validate_host(host: &str) -> Result<()> {
    if host.is_empty() {
        return Err(anyhow!("the ssh host is empty"));
    }
    if host.starts_with('-') {
        return Err(anyhow!(
            "ssh host `{host}` starts with `-` and would be parsed as an option"
        ));
    }
    if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(anyhow!(
            "ssh host `{host}` contains whitespace or control characters"
        ));
    }
    Ok(())
}

impl Target {
    pub fn from_config(cfg: &TargetConfig) -> Result<Target> {
        match cfg.kind {
            TargetKind::Local => Ok(Target::Local),
            TargetKind::Ssh => {
                let host = cfg
                    .host
                    .clone()
                    .ok_or_else(|| anyhow!("target `{}` has kind ssh but no host", cfg.name))?;
                validate_host(&host).with_context(|| format!("target `{}`", cfg.name))?;
                Ok(Target::Ssh {
                    name: cfg.name.clone(),
                    host,
                    remote_root: cfg
                        .remote_root
                        .clone()
                        .unwrap_or_else(|| "~/analyst".to_string()),
                    python: cfg.python.clone().unwrap_or_else(|| "3.12".to_string()),
                    sandbox: cfg.sandbox.clone().unwrap_or_else(|| "auto".to_string()),
                    data_dirs: cfg.data_dirs.clone(),
                    uv_binary: cfg.uv_binary.clone().filter(|p| !p.is_empty()),
                })
            }
        }
    }

    /// The launcher runs use on this target. Always [`Launcher::Direct`] for now; see
    /// [`Launcher`] for where a scheduler launcher would plug in.
    pub fn launcher(&self) -> Launcher {
        Launcher::Direct
    }

    pub fn name(&self) -> &str {
        match self {
            Target::Local => "local",
            Target::Ssh { name, .. } => name,
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Target::Ssh { .. })
    }

    pub fn host(&self) -> Option<&str> {
        match self {
            Target::Local => None,
            Target::Ssh { host, .. } => Some(host),
        }
    }

    pub fn data_dirs(&self) -> &[String] {
        match self {
            Target::Local => &[],
            Target::Ssh { data_dirs, .. } => data_dirs,
        }
    }

    pub fn sandbox(&self) -> &str {
        match self {
            Target::Local => "auto",
            Target::Ssh { sandbox, .. } => sandbox,
        }
    }

    /// Mirror directory on the host:
    /// `<remote_root>/<repo name>-<8 hex of the SHA-256 of the local repo path>`, so two repos
    /// with the same directory name never share a mirror. Characters of the repo name outside
    /// `[A-Za-z0-9._-]` become `_`, which keeps the path safe for remote shells.
    pub fn mirror_dir(&self, repo: &Path) -> Option<String> {
        match self {
            Target::Local => None,
            Target::Ssh { remote_root, .. } => {
                let canon = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
                let base: String = canon
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "repo".to_string())
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                let hash = crate::util::sha256_hex(canon.to_string_lossy().as_bytes());
                let mirror = format!(
                    "{}/{}-{}",
                    remote_root.trim_end_matches('/'),
                    base.trim_start_matches('.'),
                    &hash[..8]
                );
                // An install that runs in the mirror is checked against this project.
                crate::egress::note_mirror(&mirror, repo);
                Some(mirror)
            }
        }
    }
}

/// A `tokio` command for a command line that will run in `cwd`; a bare program name is
/// resolved through [`crate::util::resolve_tool`] so tools are found even when the app was
/// not started from a shell, and children inherit the widened `PATH`.
///
/// Every command line this module runs is built here, so this is where the network gate is
/// consulted ([`crate::egress::guard_in`]): `ssh`, `scp`, and the git push only reach
/// configured SSH targets, and `uv` runs offline unless the line installs from the
/// configured package index into a project that names no other source.
fn command_for(line: &[String], cwd: Option<&Path>) -> Result<tokio::process::Command> {
    let (program, args) = line
        .split_first()
        .ok_or_else(|| anyhow!("empty command line"))?;
    let guard = crate::egress::guard_in(line, cwd)?;
    let mut cmd = tokio::process::Command::new(crate::util::resolve_tool(program));
    cmd.args(args)
        .env("PATH", crate::util::tool_path_env())
        .kill_on_drop(true);
    guard.apply_tokio(&mut cmd);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    Ok(cmd)
}

fn check_status(line: &[String], out: &std::process::Output) -> Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // `uv` without an interpreter to use: say which one to install, and where.
    let machine = match line.iter().position(|a| a == "--") {
        Some(i) if line[0] == "ssh" => line.get(i + 1).cloned(),
        _ => Some("this machine".to_string()),
    };
    if let Some(message) = machine.and_then(|m| crate::egress::explain_missing_python(&stderr, &m))
    {
        return Err(anyhow!(message));
    }
    Err(anyhow!(
        "`{}` failed ({}): {}",
        line.join(" "),
        out.status,
        stderr.trim()
    ))
}

/// Run a command line, capturing output; errors on non-zero exit.
pub async fn run_command(line: &[String], cwd: Option<&Path>) -> Result<std::process::Output> {
    run_command_stdin(line, cwd, None).await
}

/// [`run_command`] with bytes fed to the child's stdin. Stdin is written concurrently with
/// reading the output, so neither side can block the other.
pub async fn run_command_stdin(
    line: &[String],
    cwd: Option<&Path>,
    input: Option<Vec<u8>>,
) -> Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = command_for(line, cwd)?;
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("running `{}`", line.join(" ")))?;
    let writer = match (input, child.stdin.take()) {
        (Some(bytes), Some(mut stdin)) => Some(tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        })),
        _ => None,
    };
    let out = child
        .wait_with_output()
        .await
        .with_context(|| format!("running `{}`", line.join(" ")))?;
    if let Some(w) = writer {
        let _ = w.await;
    }
    check_status(line, &out)?;
    Ok(out)
}

/// Pipe `producer`'s stdout into `consumer`'s stdin (both run with `cwd`), with bytes fed to
/// the producer's stdin. Used to bring rendered artifacts back from a host.
pub async fn pipe_commands_with_input(
    input: Option<Vec<u8>>,
    producer: &[String],
    consumer: &[String],
    cwd: Option<&Path>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut prod = command_for(producer, cwd)?;
    prod.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut prod = prod
        .spawn()
        .with_context(|| format!("spawning `{}`", producer.join(" ")))?;
    let stdout = prod
        .stdout
        .take()
        .ok_or_else(|| anyhow!("producer stdout not captured"))?;
    let writer = match (input, prod.stdin.take()) {
        (Some(bytes), Some(mut stdin)) => Some(tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        })),
        _ => None,
    };
    let mut cons = command_for(consumer, cwd)?;
    cons.stdin(Stdio::from(stdout.into_owned_fd()?))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let cons = cons
        .spawn()
        .with_context(|| format!("spawning `{}`", consumer.join(" ")))?;
    let (cons_out, prod_out) = tokio::join!(cons.wait_with_output(), prod.wait_with_output());
    if let Some(w) = writer {
        let _ = w.await;
    }
    check_status(producer, &prod_out?)?;
    check_status(consumer, &cons_out?)?;
    Ok(())
}

/// `ssh <opts> -- <host> <remote command>` as a command line. The remote command is run
/// with [`crate::egress::remote_env_prefix`] in front, so no `uv` it starts can download a
/// Python interpreter.
pub fn ssh_command(host: &str, control_dir: Option<&Path>, remote: &str) -> Vec<String> {
    let mut v = crate::kernel::transport::ssh_prefix(host, control_dir);
    v.push(format!("{}{remote}", crate::egress::remote_env_prefix()));
    v
}

/// Run a remote shell command over ssh, returning stdout.
pub async fn run_ssh(host: &str, control_dir: Option<&Path>, remote: &str) -> Result<String> {
    validate_host(host)?;
    let out = run_command(&ssh_command(host, control_dir, remote), None).await?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub mod sync {
    //! Repo sync: local canonical, remote mirror (PROTOCOL 7 and 9).
    //!
    //! The mirror is kept by git. The working tree is written to a tree object with the
    //! checkpoint machinery ([`crate::checkpoint`]), wrapped in a parentless commit, and
    //! pushed over SSH to `refs/analyst/sync` in a bare repository next to the mirror
    //! (`<mirror>.git`). The mirror directory is a detached checkout of that commit. The
    //! tree id is the sync identity: when the host has it checked out, nothing is sent.

    use super::*;
    use analyst_protocol::kernel::Artifact;

    /// One file of the synced tree. Kept for callers that still derive the tree identity
    /// from a file list ([`file_list`] then [`tree_hash`]); new code calls [`tree_id`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FileEntry {
        pub path: String,
        pub blob: String,
        /// Id of the tree this entry was listed from.
        pub tree: String,
    }

    /// The id git gives the tree with no entries.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    /// Directories at the top of the mirror that belong to the host and are never part of
    /// the pushed tree, so a checkout neither writes nor deletes anything in them: the
    /// project's output directory, the session environment, and the app's own state.
    pub fn host_owned_dirs(out_dir: &str) -> Vec<String> {
        let mut dirs = vec![".venv".to_string(), ".analyst".to_string()];
        if let Ok(out) = crate::util::normalize_rel_path(out_dir) {
            if !out.is_empty() && !dirs.contains(&out) {
                dirs.insert(0, out);
            }
        }
        dirs
    }

    fn out_dir_for(repo: &Path, target_name: &str) -> String {
        let project = crate::config::load_project(repo).unwrap_or_default();
        crate::config::effective_project(&project, target_name).out_dir
    }

    /// The tree a mirror of `repo` holds: the checkpoint tree (tracked and untracked files,
    /// not ignored, without data directories and large files) minus the repo's `.env` (keys
    /// for the app, which never leave this machine) and the [`host_owned_dirs`].
    pub fn snapshot(repo: &Path, out_dir: &str) -> Result<crate::checkpoint::Snapshot> {
        let mut drop = host_owned_dirs(out_dir);
        let mut exclude = crate::checkpoint::excluded_dirs(repo);
        exclude.extend(drop.iter().cloned());
        drop.push(wb_net::secrets::ENV_FILE.to_string());
        crate::checkpoint::worktree_tree_without(repo, &exclude, &drop)
    }

    /// The id of the tree a mirror of `repo` would hold now: the identity recorded with a
    /// run, on local and remote targets alike.
    pub fn tree_id(repo: &Path) -> Result<String> {
        Ok(snapshot(repo, &out_dir_for(repo, "local"))?.tree)
    }

    fn tree_paths(repo: &Path, tree: &str) -> Result<Vec<(String, String)>> {
        // "<mode> <type> <sha>\t<path>" records, NUL-separated.
        let out = crate::checkpoint::git(repo, &["ls-tree", "-r", "-z", tree], None)?;
        Ok(out
            .split('\0')
            .filter_map(|rec| rec.split_once('\t'))
            .map(|(meta, path)| {
                (
                    path.to_string(),
                    meta.rsplit(' ').next().unwrap_or("").to_string(),
                )
            })
            .collect())
    }

    /// Every file of the synced tree. See [`FileEntry`].
    pub fn file_list(repo: &Path) -> Result<Vec<FileEntry>> {
        let tree = tree_id(repo)?;
        Ok(tree_paths(repo, &tree)?
            .into_iter()
            .map(|(path, blob)| FileEntry {
                path,
                blob,
                tree: tree.clone(),
            })
            .collect())
    }

    /// The tree id a [`file_list`] was read from. See [`FileEntry`].
    pub fn tree_hash(entries: &[FileEntry]) -> String {
        entries
            .first()
            .map(|e| e.tree.clone())
            .unwrap_or_else(|| EMPTY_TREE.to_string())
    }

    /// Path of the bare repository that backs a mirror.
    pub fn bare_repo(mirror: &str) -> String {
        format!("{}.git", mirror.trim_end_matches('/'))
    }

    const NO_GIT: &str = "analyst-no-git";

    /// Remote command run before a push. It creates the mirror directory and, beside it, a
    /// bare repository without hooks, then prints `analyst-repo <absolute path>` and
    /// `analyst-tree <id>`: the tree the mirror is a checkout of, empty when there is none
    /// (or when the mirror directory was missing). Prints `analyst-no-git` when the host has
    /// no git.
    pub fn prepare_script(mirror: &str) -> String {
        let m = sh_quote_path(mirror);
        let b = sh_quote_path(&bare_repo(mirror));
        let env = crate::egress::REMOTE_GIT_ENV;
        format!(
            "set -e; command -v git >/dev/null 2>&1 || {{ echo {NO_GIT}; exit 0; }}; \
had=; [ -d {m} ] && had=1; mkdir -p {m}; \
[ -d {b}/objects ] || {env} git init -q --bare --template= {b}; \
b=$(cd {b} && pwd); echo \"analyst-repo $b\"; t=; \
[ -z \"$had\" ] || t=$({env} git --git-dir=\"$b\" rev-parse -q --verify 'HEAD^{{tree}}' 2>/dev/null || true); \
echo \"analyst-tree $t\""
        )
    }

    /// What [`prepare_script`] reported.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RemoteState {
        /// Absolute path of the bare repository on the host.
        pub repo: String,
        /// Tree the mirror is a checkout of, if any.
        pub tree: Option<String>,
    }

    pub fn parse_prepare(output: &str, host: &str) -> Result<RemoteState> {
        let field = |name: &str| {
            output
                .lines()
                .find_map(|l| l.trim().strip_prefix(name))
                .map(|v| v.trim().to_string())
        };
        if output.lines().any(|l| l.trim() == NO_GIT) {
            return Err(anyhow!(
                "`git` was not found on {host}. The repository mirror is kept with git: install git on {host} and try again"
            ));
        }
        let repo = field("analyst-repo")
            .filter(|r| r.starts_with('/'))
            .ok_or_else(|| {
                anyhow!(
                    "unexpected answer from {host} while preparing the mirror: `{}`",
                    output.trim()
                )
            })?;
        Ok(RemoteState {
            repo,
            tree: field("analyst-tree").filter(|t| !t.is_empty()),
        })
    }

    /// Remote command run after a push: make the mirror a detached checkout of `commit`.
    /// Files that left the tree are deleted, changed files are rewritten, and everything
    /// the tree never held (the output directory, `.venv/`, `.analyst/`, caches) is left
    /// alone. No hook, filter, or configuration of the host's user runs.
    pub fn checkout_script(mirror: &str, commit: &str) -> String {
        let m = sh_quote_path(mirror);
        let b = sh_quote_path(&bare_repo(mirror));
        format!(
            "set -e; b=$(cd {b} && pwd); m=$(cd {m} && pwd); \
{env} git -c core.hooksPath=/dev/null -c core.fsmonitor= -c advice.detachedHead=false \
--git-dir=\"$b\" --work-tree=\"$m\" checkout -q -f --detach {commit}",
            env = crate::egress::REMOTE_GIT_ENV,
            commit = sh_quote(commit),
        )
    }

    /// A scratch bare repository that borrows `repo`'s objects. The push runs from it, so
    /// nothing in the project's own `.git/config` (remotes, URL rewrites, helpers) applies.
    pub fn scratch_repo(repo: &Path) -> Result<tempfile::TempDir> {
        let objects = crate::checkpoint::git(repo, &["rev-parse", "--git-path", "objects"], None)?;
        let objects = std::path::absolute(repo.join(objects.trim()))
            .with_context(|| format!("locating the object store of {}", repo.display()))?;
        let dir = tempfile::Builder::new()
            .prefix("analyst-sync-")
            .tempdir()
            .context("creating a scratch repository for the push")?;
        crate::checkpoint::git(
            dir.path(),
            &["init", "-q", "--bare", "--template=", "."],
            None,
        )?;
        let info = dir.path().join("objects").join("info");
        std::fs::create_dir_all(&info)?;
        std::fs::write(info.join("alternates"), format!("{}\n", objects.display()))?;
        Ok(dir)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Method {
        /// The tree was pushed with git and checked out on the host.
        Git,
        /// The host already had this tree checked out.
        Skipped,
    }

    #[derive(Debug, Clone)]
    pub struct SyncReport {
        /// Id of the synced git tree.
        pub tree_hash: String,
        pub files: usize,
        pub method: Method,
        pub warning: Option<String>,
    }

    /// Bring the mirror of `repo` on `target` in line with the working tree. Nothing is sent
    /// when the mirror is already a checkout of the current tree, unless `force` is set.
    /// `last_hash` is accepted for callers that track the last synced tree; the host is
    /// always asked, since only it knows what the mirror holds.
    pub async fn push(
        repo: &Path,
        target: &Target,
        control_dir: Option<&Path>,
        _last_hash: Option<&str>,
        force: bool,
    ) -> Result<SyncReport> {
        let (host, mirror) = match target {
            Target::Ssh { host, .. } => (
                host.as_str(),
                target
                    .mirror_dir(repo)
                    .ok_or_else(|| anyhow!("no mirror dir"))?,
            ),
            Target::Local => return Err(anyhow!("the local target has no mirror to sync")),
        };
        validate_host(host)?;
        let out_dir = out_dir_for(repo, target.name());
        let repo_owned = repo.to_path_buf();
        let (snap, commit, files) = tokio::task::spawn_blocking(move || -> Result<_> {
            let snap = snapshot(&repo_owned, &out_dir)?;
            let commit =
                crate::checkpoint::commit_tree_detached(&repo_owned, &snap.tree, "analyst sync")?;
            let files = tree_paths(&repo_owned, &snap.tree)?.len();
            Ok((snap, commit, files))
        })
        .await
        .map_err(|e| anyhow!("snapshot task failed: {e}"))??;
        let state = parse_prepare(
            &run_ssh(host, control_dir, &prepare_script(&mirror)).await?,
            host,
        )?;
        if !force && state.tree.as_deref() == Some(snap.tree.as_str()) {
            return Ok(SyncReport {
                tree_hash: snap.tree,
                files,
                method: Method::Skipped,
                warning: None,
            });
        }
        let scratch = {
            let repo = repo.to_path_buf();
            tokio::task::spawn_blocking(move || scratch_repo(&repo))
                .await
                .map_err(|e| anyhow!("scratch repository task failed: {e}"))??
        };
        let line =
            crate::egress::git_push(scratch.path(), host, &state.repo, &commit, control_dir)?;
        run_command(&line, Some(scratch.path()))
            .await
            .with_context(|| format!("pushing the working tree to {host}"))?;
        run_ssh(host, control_dir, &checkout_script(&mirror, &commit))
            .await
            .with_context(|| format!("checking out the mirror on {host}"))?;
        let warning = (!snap.skipped_large.is_empty()).then(|| {
            format!(
                "{} file(s) larger than {} MB were not sent to the mirror in their current state (first: {})",
                snap.skipped_large.len(),
                crate::checkpoint::MAX_CHECKPOINT_FILE_BYTES / (1024 * 1024),
                snap.skipped_large[0]
            )
        });
        Ok(SyncReport {
            tree_hash: snap.tree,
            files,
            method: Method::Git,
            warning,
        })
    }

    /// Remote half of a pull: reads NUL-separated mirror-relative paths on stdin and writes a
    /// tar stream of those that exist.
    pub fn pull_remote_script(mirror: &str) -> String {
        format!(
            "cd {} && xargs -0 sh -c 'for f in \"$@\"; do if [ -f \"$f\" ]; then printf \"%s\\0\" \"$f\"; fi; done' sh | tar -cf - --null -T -",
            sh_quote_path(mirror)
        )
    }

    /// Consumer of a pull: `tar -xf - -C <local root>`.
    pub fn tar_extract_local_command(local_root: &Path) -> Vec<String> {
        vec![
            "tar".into(),
            "-xf".into(),
            "-".into(),
            "-C".into(),
            local_root.display().to_string(),
        ]
    }

    /// Mirror-relative paths worth pulling for one run: every artifact with `rendered: true`,
    /// its figure sidecar (`<file>.meta.json`), and the run's `run.json`. Nothing else leaves
    /// the host (G8).
    pub fn rendered_paths(artifacts: &[Artifact], run_out_dir: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |p: String| {
            if !out.contains(&p) {
                out.push(p);
            }
        };
        for a in artifacts.iter().filter(|a| a.rendered) {
            add(a.path.clone());
            if a.kind == analyst_protocol::kernel::ArtifactKind::Figure {
                add(sidecar_path(&a.path));
            }
        }
        if !run_out_dir.is_empty() {
            add(format!("{}/run.json", run_out_dir.trim_end_matches('/')));
        }
        out
    }

    /// Figure sidecar name: `<file name>.meta.json` (PROTOCOL 9).
    pub fn sidecar_path(figure: &str) -> String {
        format!("{figure}.meta.json")
    }

    /// Pull exactly `rel_paths` (mirror-relative; missing ones are skipped) from the mirror
    /// into `local_root`, preserving relative paths, with one tar stream over ssh.
    pub async fn pull_rendered(
        host: &str,
        mirror: &str,
        rel_paths: &[String],
        local_root: &Path,
        control_dir: Option<&Path>,
    ) -> Result<()> {
        validate_host(host)?;
        let producer = ssh_command(host, control_dir, &pull_remote_script(mirror));
        pull_with(&producer, rel_paths, local_root).await
    }

    /// The pull pipe with an arbitrary producer command (tests run the remote half locally).
    pub async fn pull_with(
        producer: &[String],
        rel_paths: &[String],
        local_root: &Path,
    ) -> Result<()> {
        let mut input = Vec::new();
        for p in rel_paths {
            let n = crate::util::normalize_rel_path(p).map_err(|e| anyhow!(e))?;
            if n.contains('\n') {
                continue;
            }
            input.extend_from_slice(n.as_bytes());
            input.push(0);
        }
        if input.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(local_root)
            .with_context(|| format!("creating {}", local_root.display()))?;
        pipe_commands_with_input(
            Some(input),
            producer,
            &tar_extract_local_command(local_root),
            None,
        )
        .await
    }

    /// Pull one file from the mirror into `local` (parent dirs created).
    pub async fn pull_artifact(
        host: &str,
        mirror: &str,
        path: &str,
        local: &Path,
        control_dir: Option<&Path>,
    ) -> Result<()> {
        validate_host(host)?;
        let rel = crate::util::normalize_rel_path(path).map_err(|e| anyhow!(e))?;
        let remote = format!("cat -- {}/{}", sh_quote_path(mirror), sh_quote(&rel));
        let out = run_command(&ssh_command(host, control_dir, &remote), None).await?;
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(local, out.stdout).with_context(|| format!("writing {}", local.display()))
    }
}

pub mod bootstrap {
    //! Tooling bootstrap: the remote sequence (PROTOCOL 7) and the local tooling environment
    //! under `$ANALYST_HOME/tooling`, both idempotent and version-checked.

    use super::*;

    /// Version written to the tooling `VERSION` file: the core version.
    pub fn tooling_version() -> &'static str {
        crate::CORE_VERSION
    }

    /// Remote command that prints the installed tooling version (empty when absent).
    pub fn version_check_command() -> String {
        "cat ~/.analyst/tooling/VERSION 2>/dev/null || true".to_string()
    }

    /// Remote command that makes `~/.analyst/bin/uv` point at a `uv` already on the host, if
    /// there is one, and reports what it found. It prints two lines: `have` (already in
    /// place), `linked` (found elsewhere and linked), or `missing`; then `uname -sm`.
    pub fn uv_probe_command() -> String {
        "mkdir -p ~/.analyst/bin ~/.analyst/tooling; \
if [ -x ~/.analyst/bin/uv ]; then echo have; else \
found=; for c in \"$(command -v uv 2>/dev/null)\" ~/.local/bin/uv ~/.cargo/bin/uv /usr/local/bin/uv /opt/homebrew/bin/uv; do \
if [ -n \"$c\" ] && [ -x \"$c\" ]; then found=$c; break; fi; done; \
if [ -n \"$found\" ]; then ln -sf \"$found\" ~/.analyst/bin/uv && echo linked; else echo missing; fi; fi; uname -sm"
            .to_string()
    }

    /// What [`uv_probe_command`] reported.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UvProbe {
        /// `uv` is available at `~/.analyst/bin/uv`.
        pub present: bool,
        /// `uname -sm` of the host, e.g. `Linux x86_64`.
        pub platform: String,
    }

    pub fn parse_uv_probe(output: &str) -> Result<UvProbe> {
        let mut lines = output.lines().map(str::trim).filter(|l| !l.is_empty());
        let state = lines.next().unwrap_or("");
        let platform = lines.next().unwrap_or("").to_string();
        match state {
            "have" | "linked" => Ok(UvProbe {
                present: true,
                platform,
            }),
            "missing" => Ok(UvProbe {
                present: false,
                platform,
            }),
            other => Err(anyhow!(
                "unexpected answer from the host while looking for uv: `{other}`"
            )),
        }
    }

    /// `uname -sm` of this machine, or `None` when it cannot be asked.
    pub fn local_platform() -> Option<String> {
        let out = std::process::Command::new("uname")
            .arg("-sm")
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !s.is_empty()).then_some(s)
    }

    /// The local file to copy to a host that has no `uv`: the local `uv` when the host is
    /// the same OS and architecture as this machine, else the target's configured
    /// `uv_binary`. Nothing is ever downloaded; when neither applies the error says what to
    /// provide.
    pub fn uv_binary_for(
        target_name: &str,
        remote_platform: &str,
        local_platform: Option<&str>,
        local_uv: Option<&Path>,
        configured: Option<&str>,
    ) -> Result<PathBuf> {
        let same = !remote_platform.is_empty() && local_platform == Some(remote_platform);
        if same {
            if let Some(uv) = local_uv {
                return Ok(uv.to_path_buf());
            }
        }
        if let Some(path) = configured {
            let path = crate::util::expand_tilde(path);
            if path.is_file() {
                return Ok(path);
            }
            return Err(anyhow!(
                "`uv_binary` for target `{target_name}` is {}, which is not a file",
                path.display()
            ));
        }
        let remote = if remote_platform.is_empty() {
            "an unknown platform"
        } else {
            remote_platform
        };
        let why = if same {
            "no local `uv` was found to copy".to_string()
        } else {
            format!(
                "this machine is {}, so its own `uv` cannot be copied",
                local_platform.unwrap_or("an unknown platform")
            )
        };
        Err(anyhow!(
            "the host of target `{target_name}` ({remote}) has no `uv`, and {why}. The app does not download it. Place a `uv` binary built for {remote} on this machine and set `uv_binary = \"/path/to/uv\"` for the target in config.toml, or install `uv` on the host yourself"
        ))
    }

    /// `scp -q <opts> -- <local uv> <host>:.analyst/bin/uv` (a relative remote path is
    /// resolved against the home directory).
    pub fn scp_uv_command(host: &str, uv: &Path, control_dir: Option<&Path>) -> Vec<String> {
        let mut v = vec!["scp".to_string(), "-q".to_string()];
        v.extend(crate::kernel::transport::ssh_options(control_dir));
        v.push("--".into());
        v.push(uv.display().to_string());
        v.push(format!("{host}:.analyst/bin/uv"));
        v
    }

    /// Make sure `~/.analyst/bin/uv` exists on the host: use a `uv` that is already there,
    /// else copy one over SSH. No installer is fetched.
    pub async fn ensure_remote_uv(target: &Target, control_dir: Option<&Path>) -> Result<()> {
        let Target::Ssh {
            name,
            host,
            uv_binary,
            ..
        } = target
        else {
            return Ok(());
        };
        let probe = parse_uv_probe(&run_ssh(host, control_dir, &uv_probe_command()).await?)?;
        if probe.present {
            return Ok(());
        }
        let local_uv = crate::util::find_tool("uv");
        let binary = uv_binary_for(
            name,
            &probe.platform,
            local_platform().as_deref(),
            local_uv.as_deref(),
            uv_binary.as_deref(),
        )?;
        run_command(&scp_uv_command(host, &binary, control_dir), None)
            .await
            .with_context(|| format!("copying {} to {host}", binary.display()))?;
        run_ssh(
            host,
            control_dir,
            "chmod +x ~/.analyst/bin/uv && ~/.analyst/bin/uv --version",
        )
        .await
        .with_context(|| {
            format!(
                "the copied uv ({}) does not run on {host} ({}); set `uv_binary` for target `{name}` to a binary built for that host",
                binary.display(),
                probe.platform
            )
        })?;
        Ok(())
    }

    const PYTHON_FOUND: &str = "analyst-python-found";

    /// Remote command that reports whether the host has the interpreter the tooling needs.
    /// `uv` looks at what is installed there and downloads nothing.
    pub fn python_probe_command(python_version: &str) -> String {
        format!(
            "~/.analyst/bin/uv python find {} >/dev/null 2>&1 && echo {PYTHON_FOUND} || echo analyst-python-missing",
            sh_quote(python_version)
        )
    }

    /// The error for a host without the target's Python version: it names the version and
    /// says to install it there.
    pub fn check_python_probe(
        output: &str,
        python_version: &str,
        target_name: &str,
        host: &str,
    ) -> Result<()> {
        if output.lines().any(|l| l.trim() == PYTHON_FOUND) {
            return Ok(());
        }
        Err(anyhow!(
            "{}. Target `{target_name}` asks for it with `python = \"{python_version}\"` in config.toml; another version that is installed on {host} can be named there instead",
            crate::egress::python_missing(python_version, host)
        ))
    }

    /// The remote command sequence, run after `uv` is in place ([`ensure_remote_uv`]), the
    /// host's Python has been found ([`python_probe_command`]), and the kernel wheel has
    /// been copied to `wheel_remote` (a path on the host). `index_flag` is the authorized
    /// `--default-index '<url>'` from [`crate::egress::remote_index_flag`]. Python itself is
    /// never installed by this sequence.
    pub fn commands(
        python_version: &str,
        wheel_remote: &str,
        version: &str,
        index_flag: &str,
    ) -> Vec<String> {
        vec![
            "mkdir -p ~/.analyst/bin ~/.analyst/tooling".to_string(),
            "test -x ~/.analyst/bin/uv".to_string(),
            format!(
                "test -x ~/.analyst/tooling/.venv/bin/python || ~/.analyst/bin/uv venv {index_flag} --python {} ~/.analyst/tooling/.venv",
                sh_quote(python_version)
            ),
            format!(
                "~/.analyst/bin/uv pip install {index_flag} --python ~/.analyst/tooling/.venv/bin/python {}",
                sh_quote_path(wheel_remote)
            ),
            format!("printf '%s\\n' {} > ~/.analyst/tooling/VERSION", sh_quote(version)),
        ]
    }

    /// The whole sequence as one `sh -e` script line (so one ssh round trip).
    pub fn script(
        python_version: &str,
        wheel_remote: &str,
        version: &str,
        index_flag: &str,
    ) -> String {
        format!(
            "set -e; {}",
            commands(python_version, wheel_remote, version, index_flag).join("; ")
        )
    }

    /// Session venv setup in the mirror: `uv sync` when a pyproject exists, else venv +
    /// requirements. Installs come from the configured index only (`index_flag`, see
    /// [`commands`]). It first asks `uv` for an installed interpreter the project can use;
    /// without one it stops, showing which version `uv` looked for and that it has to be
    /// installed on the host.
    pub fn session_env_command(mirror: &str, index_flag: &str) -> String {
        let m = sh_quote_path(mirror);
        let hint = sh_quote(
            "The app never downloads Python interpreters. Install the Python version named above on this host yourself (its package manager is the usual way), then try again.",
        );
        format!(
            "cd {m} && {{ ~/.analyst/bin/uv python find >/dev/null || {{ echo {hint} >&2; exit 1; }}; }} && if [ -f pyproject.toml ]; then ~/.analyst/bin/uv sync {index_flag}; else ~/.analyst/bin/uv venv {index_flag} && (test -f requirements.txt && ~/.analyst/bin/uv pip install {index_flag} -r requirements.txt || true); fi"
        )
    }

    /// `scp -q <opts> -- <wheel> <host>:.analyst/tooling/<name>` (a relative remote path is
    /// resolved against the home directory, so no remote shell expansion is involved).
    pub fn scp_wheel_command(host: &str, wheel: &Path, control_dir: Option<&Path>) -> Vec<String> {
        let name: String = wheel
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "analyst_kernel.whl".into())
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let mut v = vec!["scp".to_string(), "-q".to_string()];
        v.extend(crate::kernel::transport::ssh_options(control_dir));
        v.push("--".into());
        v.push(wheel.display().to_string());
        v.push(format!("{host}:.analyst/tooling/{name}"));
        v
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Outcome {
        UpToDate(String),
        Installed(String),
    }

    /// Check the version file and run the sequence when missing or different.
    pub async fn ensure(
        target: &Target,
        control_dir: Option<&Path>,
        wheel: Option<&Path>,
    ) -> Result<Outcome> {
        let (name, host, python) = match target {
            Target::Ssh {
                name, host, python, ..
            } => (name.as_str(), host.as_str(), python.as_str()),
            Target::Local => return Err(anyhow!("the local target needs no bootstrap")),
        };
        let want = tooling_version();
        let have = run_ssh(host, control_dir, &version_check_command()).await?;
        if have.trim() == want {
            return Ok(Outcome::UpToDate(want.to_string()));
        }
        let wheel = wheel.ok_or_else(|| {
            anyhow!(
                "no kernel wheel found; build one with `uv build` in python/kernel (dist/*.whl)"
            )
        })?;
        ensure_remote_uv(target, control_dir).await?;
        check_python_probe(
            &run_ssh(host, control_dir, &python_probe_command(python)).await?,
            python,
            name,
            host,
        )?;
        let index_flag = crate::egress::remote_index_flag("tooling bootstrap (remote)")?;
        let scp = scp_wheel_command(host, wheel, control_dir);
        run_command(&scp, None).await?;
        let remote_wheel = scp
            .last()
            .and_then(|s| s.split_once(':'))
            .map(|(_, p)| format!("~/{p}"))
            .unwrap_or_default();
        run_ssh(
            host,
            control_dir,
            &script(python, &remote_wheel, want, &index_flag),
        )
        .await?;
        Ok(Outcome::Installed(want.to_string()))
    }

    /// The local tooling environment's interpreter: `<home>/tooling/.venv/bin/python`.
    pub fn local_tooling_python(home: &Path) -> PathBuf {
        let venv = home.join("tooling").join(".venv");
        if cfg!(windows) {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python")
        }
    }

    /// Whether [`ensure_local`] has nothing to do.
    pub fn local_is_current(home: &Path) -> bool {
        local_tooling_python(home).is_file()
            && std::fs::read_to_string(home.join("tooling").join("VERSION"))
                .map(|v| v.trim() == tooling_version())
                .unwrap_or(false)
    }

    /// Create or refresh the local tooling environment: `uv venv <home>/tooling/.venv` and
    /// `uv pip install <kernel_source_dir>` when the interpreter is missing or the `VERSION`
    /// file differs from the core version. `kernel_source_dir` is a directory or wheel that
    /// provides `analyst_kernel` (bundled with the app, or `python/kernel` in a checkout).
    pub async fn ensure_local(home: &Path, kernel_source_dir: &Path) -> Result<Outcome> {
        let want = tooling_version();
        if local_is_current(home) {
            return Ok(Outcome::UpToDate(want.to_string()));
        }
        let uv = crate::util::find_tool("uv").ok_or_else(|| {
            anyhow!(
                "`uv` was not found (looked on PATH, in ~/.local/bin, ~/.cargo/bin, /opt/homebrew/bin, /usr/local/bin); install uv to set up the tooling environment (the app does not download it)"
            )
        })?;
        if !kernel_source_dir.exists() {
            return Err(anyhow!(
                "kernel source {} does not exist",
                kernel_source_dir.display()
            ));
        }
        let uv = uv.to_string_lossy().to_string();
        let tooling = home.join("tooling");
        let venv = tooling.join(".venv");
        std::fs::create_dir_all(&tooling)
            .with_context(|| format!("creating {}", tooling.display()))?;
        // The kernel says which Python it needs; uv must find one that is installed.
        let python = kernel_requires_python(kernel_source_dir);
        {
            let (dir, request) = (tooling.clone(), python.clone());
            tokio::task::spawn_blocking(move || {
                crate::egress::require_python(&dir, request.as_deref())
            })
            .await
            .map_err(|e| anyhow!("interpreter check failed: {e}"))?
            .context("setting up the tooling environment")?;
        }
        let mut venv_args = vec!["-q".to_string()];
        if let Some(request) = &python {
            venv_args.extend(["--python".to_string(), request.clone()]);
        }
        venv_args.push(venv.to_string_lossy().to_string());
        let _ = std::fs::remove_file(tooling.join("VERSION"));
        if venv.exists() {
            // Start clean: uv versions differ on whether an existing venv is replaced.
            std::fs::remove_dir_all(&venv)
                .with_context(|| format!("removing {}", venv.display()))?;
        }
        // Both steps may fetch from the package index (the kernel's dependencies), so both
        // name the configured index and are authorized by the gate when spawned.
        run_command(
            &crate::egress::uv_install(&uv, &["venv"], &venv_args),
            Some(&tooling),
        )
        .await
        .context("creating the tooling environment")?;
        let python = local_tooling_python(home);
        run_command(
            &crate::egress::uv_install(
                &uv,
                &["pip", "install"],
                &[
                    "-q".to_string(),
                    "--python".to_string(),
                    python.to_string_lossy().to_string(),
                    kernel_source_dir.to_string_lossy().to_string(),
                ],
            ),
            Some(&tooling),
        )
        .await
        .context("installing the kernel into the tooling environment")?;
        std::fs::write(tooling.join("VERSION"), format!("{want}\n"))
            .with_context(|| format!("writing {}", tooling.join("VERSION").display()))?;
        Ok(Outcome::Installed(want.to_string()))
    }

    /// `requires-python` of the kernel source (a directory with a `pyproject.toml`), e.g.
    /// `>=3.11`; `None` for a wheel or when the file does not say.
    pub fn kernel_requires_python(kernel_source_dir: &Path) -> Option<String> {
        let text = std::fs::read_to_string(kernel_source_dir.join("pyproject.toml")).ok()?;
        let doc: toml::Value = toml::from_str(&text).ok()?;
        doc.get("project")?
            .get("requires-python")?
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn search_starts() -> Vec<PathBuf> {
        let mut starts = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            starts.push(cwd);
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(p) = exe.parent() {
                starts.push(p.to_path_buf());
            }
        }
        starts
    }

    /// Locate the kernel source in dev mode: `<workspace>/python/kernel` (has a
    /// `pyproject.toml`). Packaged apps pass their bundled resource directory instead.
    pub fn find_dev_kernel_source() -> Option<PathBuf> {
        for s in search_starts() {
            for dir in s.ancestors() {
                let k = dir.join("python/kernel");
                if k.join("pyproject.toml").is_file() {
                    return Some(k);
                }
            }
        }
        None
    }

    /// Locate a kernel wheel in dev mode: `<workspace>/python/kernel/dist/*.whl`.
    pub fn find_dev_wheel() -> Option<PathBuf> {
        for s in search_starts() {
            for dir in s.ancestors() {
                let dist = dir.join("python/kernel/dist");
                if let Ok(rd) = std::fs::read_dir(&dist) {
                    let mut wheels: Vec<PathBuf> = rd
                        .filter_map(|e| e.ok().map(|e| e.path()))
                        .filter(|p| p.extension().map(|e| e == "whl").unwrap_or(false))
                        .collect();
                    wheels.sort();
                    if let Some(w) = wheels.pop() {
                        return Some(w);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::sync::*;
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        crate::checkpoint::git(repo, args, None).unwrap();
    }

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join(".gitignore"), "out/\n.venv/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("scripts/a.py"), "print(1)\n").unwrap();
        std::fs::write(dir.path().join("out/ignored.txt"), "x").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.path().join("untracked.md"), "hello\n").unwrap();
        dir
    }

    fn sh(script: String) -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), script]
    }

    fn ssh_target(root: &str) -> Target {
        Target::Ssh {
            name: "lab".into(),
            host: "lab-gpu".into(),
            remote_root: root.into(),
            python: "3.12".into(),
            sandbox: "auto".into(),
            data_dirs: vec![],
            uv_binary: None,
        }
    }

    fn tree_files(repo: &Path, tree: &str) -> Vec<String> {
        crate::checkpoint::git(repo, &["ls-tree", "-r", "--name-only", tree], None)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The synced tree is the working tree as git sees it, minus `.env` and the directories
    /// the host owns, whether or not those are tracked or ignored.
    #[test]
    fn snapshot_leaves_out_env_and_host_owned_directories() {
        let repo = temp_repo();
        let root = repo.path();
        for (p, text) in [
            (".env", "ANTHROPIC_API_KEY=sk-secret\n"),
            ("results/kept-on-host.txt", "x"),
            (".analyst/session.json", "{}"),
            (".venv/pyvenv.cfg", "venv"),
            ("sub/.env", "nested files are ordinary files\n"),
        ] {
            std::fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            std::fs::write(root.join(p), text).unwrap();
        }
        // Tracked, committed, and not ignored: still never part of the pushed tree.
        git(root, &["add", "-f", ".env", "results", ".analyst", ".venv"]);
        git(root, &["commit", "-q", "-m", "tracked by mistake"]);
        let snap = snapshot(root, "results").unwrap();
        assert_eq!(
            tree_files(root, &snap.tree),
            vec![".gitignore", "scripts/a.py", "sub/.env", "untracked.md"]
        );
        assert!(snap.skipped_large.is_empty());
        // The user's index and branch are untouched.
        let status = crate::checkpoint::git(root, &["status", "--porcelain"], None).unwrap();
        assert_eq!(status.trim(), "?? sub/\n?? untracked.md");
        let refs = crate::checkpoint::git(root, &["for-each-ref"], None).unwrap();
        assert_eq!(refs.lines().count(), 1, "{refs}");

        // Identity: stable while nothing changes, different after an edit or a deletion.
        let id = tree_id(root).unwrap();
        assert_eq!(id.len(), 40);
        assert_eq!(tree_id(root).unwrap(), id);
        let list = file_list(root).unwrap();
        assert_eq!(
            tree_hash(&list),
            id,
            "the list-based identity is the tree id"
        );
        assert!(list.iter().all(|e| e.path != ".env"));
        std::fs::write(root.join("scripts/a.py"), "print(2)\n").unwrap();
        let edited = tree_id(root).unwrap();
        assert_ne!(edited, id);
        std::fs::remove_file(root.join("scripts/a.py")).unwrap();
        let deleted = snapshot(root, "out").unwrap();
        assert!(!tree_files(root, &deleted.tree).contains(&"scripts/a.py".to_string()));
        // The sync commit has no parent and depends on the tree alone.
        let c1 = crate::checkpoint::commit_tree_detached(root, &id, "analyst sync").unwrap();
        let c2 = crate::checkpoint::commit_tree_detached(root, &id, "analyst sync").unwrap();
        assert_eq!(c1, c2);
        let parents = crate::checkpoint::git(root, &["rev-list", "--parents", "-1", &c1], None);
        assert_eq!(parents.unwrap().trim(), c1);
        // An empty repository has the empty tree.
        let empty = tempfile::tempdir().unwrap();
        git(empty.path(), &["init", "-q"]);
        assert_eq!(tree_hash(&file_list(empty.path()).unwrap()), EMPTY_TREE_ID);
        assert_eq!(tree_id(empty.path()).unwrap(), EMPTY_TREE_ID);
    }

    const EMPTY_TREE_ID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    /// B1: a few thousand changed paths must not fill a pipe and hang.
    #[test]
    fn snapshot_does_not_deadlock_on_large_repos() {
        let repo = temp_repo();
        let dir = repo
            .path()
            .join("many/deeply/nested/directory/with/a/long/prefix");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..6000 {
            std::fs::write(dir.join(format!("file_with_a_long_name_{i:05}.txt")), "x").unwrap();
        }
        let root = repo.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(file_list(&root).map(|l| l.len()));
        });
        let n = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the snapshot hung")
            .unwrap();
        assert_eq!(n, 6003);
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            bare_repo("~/analyst/repo-1a2b3c4d"),
            "~/analyst/repo-1a2b3c4d.git"
        );
        let prepare = prepare_script("~/analyst/my repo");
        assert!(
            prepare.starts_with("set -e; command -v git >/dev/null 2>&1 || { echo analyst-no-git;")
        );
        assert!(
            prepare.contains("mkdir -p ~/'analyst/my repo';"),
            "{prepare}"
        );
        assert!(
            prepare.contains(
                "GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git init -q --bare --template= ~/'analyst/my repo.git'"
            ),
            "{prepare}"
        );
        let checkout = checkout_script("~/a/r", "abc123");
        assert!(
            checkout.contains("-c core.hooksPath=/dev/null"),
            "{checkout}"
        );
        assert!(checkout.contains("GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git "));
        assert!(
            checkout.ends_with("checkout -q -f --detach abc123"),
            "{checkout}"
        );
        // What the host answers.
        let state =
            parse_prepare("analyst-repo /home/u/analyst/r.git\nanalyst-tree \n", "lab").unwrap();
        assert_eq!(state.repo, "/home/u/analyst/r.git");
        assert_eq!(state.tree, None);
        let state = parse_prepare("motd\nanalyst-repo /r.git\nanalyst-tree abc\n", "lab").unwrap();
        assert_eq!(state.tree.as_deref(), Some("abc"));
        let err = parse_prepare("analyst-no-git\n", "lab-gpu")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("`git` was not found on lab-gpu") && err.contains("install git"),
            "{err}"
        );
        assert!(parse_prepare("bash: weird", "lab").is_err());
        // Every remote command starts by forbidding Python downloads.
        let ssh = ssh_command("lab", None, "true");
        assert_eq!(
            ssh.last().unwrap(),
            "export UV_PYTHON_DOWNLOADS=never; true"
        );
        assert_eq!(
            tar_extract_local_command(Path::new("/repo")),
            vec!["tar", "-xf", "-", "-C", "/repo"]
        );
        assert!(pull_remote_script("~/a/my repo").starts_with("cd ~/'a/my repo' && xargs -0"));
    }

    /// Populate a stand-in mirror with files the host owns.
    fn seed_mirror(mirror: &Path) {
        for (p, text) in [
            ("out/fit/r1/run.json", "{}"),
            (".venv/pyvenv.cfg", "venv"),
            (".analyst/env.sh", "env"),
            ("scripts/__pycache__/a.cpython-312.pyc", "pyc"),
        ] {
            let f = mirror.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
    }

    /// One sync with the host's half run by a local shell and the push going to a local
    /// path: the same scripts, tree, and commit as over SSH. Returns the method used.
    async fn sync_locally(repo: &Path, mirror: &Path) -> Method {
        let m = mirror.display().to_string();
        let snap = snapshot(repo, "out").unwrap();
        let commit =
            crate::checkpoint::commit_tree_detached(repo, &snap.tree, "analyst sync").unwrap();
        let out = run_command(&sh(prepare_script(&m)), None).await.unwrap();
        let state = parse_prepare(&String::from_utf8_lossy(&out.stdout), "here").unwrap();
        if state.tree.as_deref() == Some(snap.tree.as_str()) {
            return Method::Skipped;
        }
        let scratch = scratch_repo(repo).unwrap();
        crate::checkpoint::git(
            scratch.path(),
            &[
                "push",
                "-q",
                "--force",
                &state.repo,
                &format!("{commit}:{}", crate::egress::SYNC_REF),
            ],
            None,
        )
        .unwrap();
        run_command(&sh(checkout_script(&m, &commit)), None)
            .await
            .unwrap();
        Method::Git
    }

    /// R1 and the sync contract, without a network: first sync, no-op resync, deletion,
    /// host-owned directories untouched, `.env` never sent.
    #[tokio::test]
    async fn mirror_is_a_checkout_of_the_pushed_tree() {
        let repo = temp_repo();
        let root = repo.path();
        std::fs::write(root.join(".env"), "KEY=sk-secret-value\n").unwrap();
        std::fs::write(root.join("scripts/old.py"), "print('old')\n").unwrap();
        let dest = tempfile::tempdir().unwrap();
        // A mirror path with a space exercises the quoting of every remote script.
        let mirror = dest.path().join("my mirror");
        seed_mirror(&mirror);
        assert_eq!(sync_locally(root, &mirror).await, Method::Git);
        assert_eq!(
            std::fs::read_to_string(mirror.join("scripts/a.py")).unwrap(),
            "print(1)\n"
        );
        assert!(mirror.join("untracked.md").is_file());
        assert!(mirror.join("scripts/old.py").is_file());
        assert!(
            !mirror.join("out/ignored.txt").exists(),
            "ignored files stay"
        );
        assert!(
            !mirror.join(".git").exists(),
            "the mirror is a plain directory"
        );
        let bare = dest.path().join("my mirror.git");
        assert!(bare.join("objects").is_dir());
        assert!(
            !bare.join("hooks").exists(),
            "the bare repository has no hooks"
        );
        // `.env` is neither in the mirror nor anywhere in the host's object store.
        assert!(!mirror.join(".env").exists());
        let objects = crate::checkpoint::git(
            &bare,
            &["cat-file", "--batch-all-objects", "--batch-check"],
            None,
        )
        .unwrap();
        let env_blob = crate::checkpoint::git(root, &["hash-object", ".env"], None).unwrap();
        assert!(!objects.contains(env_blob.trim()), "{objects}");
        // Only refs/analyst/sync exists on the host.
        let refs = crate::checkpoint::git(&bare, &["for-each-ref", "--format=%(refname)"], None);
        assert_eq!(refs.unwrap().trim(), crate::egress::SYNC_REF);

        // Nothing changed: nothing is sent.
        assert_eq!(sync_locally(root, &mirror).await, Method::Skipped);

        // An edit and a deletion arrive; what the host owns is untouched.
        std::fs::remove_file(root.join("scripts/old.py")).unwrap();
        std::fs::write(root.join("scripts/a.py"), "print(2)\n").unwrap();
        std::fs::create_dir_all(root.join("analysis/old")).unwrap();
        std::fs::write(root.join("analysis/old/mod.py"), "x = 1\n").unwrap();
        assert_eq!(sync_locally(root, &mirror).await, Method::Git);
        std::fs::remove_dir_all(root.join("analysis")).unwrap();
        assert_eq!(sync_locally(root, &mirror).await, Method::Git);
        assert_eq!(
            std::fs::read_to_string(mirror.join("scripts/a.py")).unwrap(),
            "print(2)\n"
        );
        assert!(!mirror.join("scripts/old.py").exists(), "deleted locally");
        assert!(
            !mirror.join("analysis").exists(),
            "emptied dirs are removed"
        );
        assert!(mirror.join("out/fit/r1/run.json").is_file(), "out_dir kept");
        assert!(mirror.join(".venv/pyvenv.cfg").is_file(), ".venv kept");
        assert!(mirror.join(".analyst/env.sh").is_file(), ".analyst kept");
        assert!(mirror
            .join("scripts/__pycache__/a.cpython-312.pyc")
            .is_file());
        assert!(!mirror.join(".env").exists());
        // The user's repository gained no ref and its index is as it was.
        let refs = crate::checkpoint::git(root, &["for-each-ref"], None).unwrap();
        assert_eq!(refs.lines().count(), 1, "{refs}");

        // A mirror directory that was removed on the host is restored, not skipped.
        std::fs::remove_dir_all(&mirror).unwrap();
        assert_eq!(sync_locally(root, &mirror).await, Method::Git);
        assert!(mirror.join("scripts/a.py").is_file());
    }

    /// R6: every ssh call carries timeouts and keepalives, `--` precedes the host, and hosts
    /// that look like options are rejected.
    #[test]
    fn ssh_options_and_host_validation() {
        let line = ssh_command("lab", None, "true");
        for opt in [
            "BatchMode=yes",
            "ConnectTimeout=10",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
        ] {
            assert!(line.iter().any(|a| a == opt), "{opt} missing in {line:?}");
        }
        assert_eq!(
            line[line.len() - 3..],
            ["--", "lab", "export UV_PYTHON_DOWNLOADS=never; true"]
        );
        assert!(validate_host("lab-gpu").is_ok());
        assert!(validate_host("user@host.example").is_ok());
        for bad in ["-oProxyCommand=evil", "", "a b", "a\nb"] {
            assert!(validate_host(bad).is_err(), "{bad:?}");
        }
        let cfg = TargetConfig {
            name: "evil".into(),
            kind: TargetKind::Ssh,
            host: Some("-oProxyCommand=touch /tmp/x".into()),
            remote_root: None,
            python: None,
            sandbox: None,
            data_dirs: vec![],
            uv_binary: None,
        };
        assert!(Target::from_config(&cfg).is_err());
    }

    /// uv reaches a host by being there already or by being copied, never by download.
    #[test]
    fn remote_uv_is_found_or_copied() {
        use bootstrap::{parse_uv_probe, uv_binary_for, UvProbe};
        assert_eq!(
            parse_uv_probe("have\nLinux x86_64\n").unwrap(),
            UvProbe {
                present: true,
                platform: "Linux x86_64".into()
            }
        );
        assert!(parse_uv_probe("linked\nLinux aarch64").unwrap().present);
        let missing = parse_uv_probe("missing\nLinux aarch64\n").unwrap();
        assert!(!missing.present);
        assert_eq!(missing.platform, "Linux aarch64");
        assert!(parse_uv_probe("bash: weird").is_err());

        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("uv");
        let other = dir.path().join("uv-aarch64");
        std::fs::write(&local, b"x").unwrap();
        std::fs::write(&other, b"y").unwrap();
        let other_s = other.to_string_lossy().to_string();
        // Same platform: the local uv.
        assert_eq!(
            uv_binary_for(
                "lab",
                "Linux x86_64",
                Some("Linux x86_64"),
                Some(&local),
                Some(&other_s)
            )
            .unwrap(),
            local
        );
        // Another platform: the configured binary, never the local one.
        assert_eq!(
            uv_binary_for(
                "lab",
                "Linux aarch64",
                Some("Darwin arm64"),
                Some(&local),
                Some(&other_s)
            )
            .unwrap(),
            other
        );
        // Neither applies: an error that says what to do, and no URL.
        let err = uv_binary_for(
            "lab",
            "Linux aarch64",
            Some("Darwin arm64"),
            Some(&local),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("uv_binary") && err.contains("Linux aarch64") && err.contains("lab"),
            "{err}"
        );
        assert!(!err.contains("http"), "{err}");
        assert!(uv_binary_for("lab", "Linux x86_64", Some("Linux x86_64"), None, None).is_err());
        assert!(uv_binary_for(
            "lab",
            "Linux aarch64",
            Some("Linux x86_64"),
            Some(&local),
            Some("/nonexistent/uv")
        )
        .is_err());

        let scp = bootstrap::scp_uv_command("me@lab", &local, None);
        assert_eq!(scp[..2], ["scp", "-q"]);
        assert_eq!(
            scp[scp.len() - 3..],
            [
                "--".to_string(),
                local.display().to_string(),
                "me@lab:.analyst/bin/uv".to_string()
            ]
        );
        assert_eq!(ssh_target("~/analyst").launcher(), Launcher::Direct);
        assert_eq!(Target::Local.launcher(), Launcher::default());
    }

    #[test]
    fn bootstrap_commands() {
        let flag = "--default-index 'https://pypi.org/simple'";
        let cmds = bootstrap::commands(
            "3.12",
            "~/.analyst/tooling/analyst_kernel-0.0.1-py3-none-any.whl",
            "0.0.1",
            flag,
        );
        assert_eq!(cmds[0], "mkdir -p ~/.analyst/bin ~/.analyst/tooling");
        // uv is provided before the script runs; the script only checks it and never
        // downloads an installer.
        assert_eq!(cmds[1], "test -x ~/.analyst/bin/uv");
        // The host's own Python is used; the sequence never installs one.
        assert!(
            cmds.iter().all(|c| !c.contains("python install")),
            "{cmds:?}"
        );
        assert!(cmds[2].contains(
            "uv venv --default-index 'https://pypi.org/simple' --python 3.12 ~/.analyst/tooling/.venv"
        ));
        assert!(cmds[3].starts_with(
            "~/.analyst/bin/uv pip install --default-index 'https://pypi.org/simple' --python ~/.analyst/tooling/.venv/bin/python ~/.analyst/tooling/analyst_kernel"
        ));
        assert_eq!(cmds[4], "printf '%s\\n' 0.0.1 > ~/.analyst/tooling/VERSION");
        let script = bootstrap::script("3.12", "w", "v", flag);
        assert!(script.starts_with("set -e; mkdir -p"));
        for text in [script.as_str(), &bootstrap::uv_probe_command()] {
            assert!(
                !text.contains("http://") && !text.contains("curl") && !text.contains("wget"),
                "{text}"
            );
            assert_eq!(text.matches("https://").count(), text.matches(flag).count());
        }
        // Without the target's Python the bootstrap stops with a message that names the
        // version and where to install it.
        assert_eq!(
            bootstrap::python_probe_command("3.12"),
            "~/.analyst/bin/uv python find 3.12 >/dev/null 2>&1 && echo analyst-python-found || echo analyst-python-missing"
        );
        bootstrap::check_python_probe("analyst-python-found\n", "3.12", "lab", "lab-gpu").unwrap();
        let err =
            bootstrap::check_python_probe("analyst-python-missing\n", "3.12", "lab", "lab-gpu")
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("no Python 3.12 interpreter was found on lab-gpu")
                && err.contains("never downloads")
                && err.contains("Install Python 3.12 on lab-gpu")
                && err.contains("python = \"3.12\""),
            "{err}"
        );
        let env = bootstrap::session_env_command("~/analyst/repo", flag);
        assert!(env.starts_with("cd ~/analyst/repo && "));
        assert!(
            env.contains("~/.analyst/bin/uv python find >/dev/null || {"),
            "{env}"
        );
        assert!(env.contains(&format!("uv sync {flag};")), "{env}");
        assert!(env.contains(&format!("uv pip install {flag} -r")), "{env}");
        assert!(bootstrap::session_env_command("~/analyst/my repo", flag)
            .starts_with("cd ~/'analyst/my repo' && "));
        let scp = bootstrap::scp_wheel_command("lab", Path::new("/w/k.whl"), None);
        assert_eq!(scp[..2], ["scp", "-q"]);
        assert!(scp.contains(&"ConnectTimeout=10".to_string()));
        assert_eq!(
            scp[scp.len() - 3..],
            ["--", "/w/k.whl", "lab:.analyst/tooling/k.whl"]
        );
        assert_eq!(
            bootstrap::version_check_command(),
            "cat ~/.analyst/tooling/VERSION 2>/dev/null || true"
        );
    }

    /// G8: only the named rendered files come back, not the run directory.
    #[tokio::test]
    async fn pull_brings_back_only_the_listed_files() {
        use analyst_protocol::kernel::{Artifact, ArtifactKind};
        let remote = tempfile::tempdir().unwrap();
        let mirror = remote.path().join("the mirror");
        let run = mirror.join("out/fit/r1");
        std::fs::create_dir_all(run.join("figures")).unwrap();
        std::fs::write(run.join("figures/fit.png"), "PNG").unwrap();
        std::fs::write(run.join("figures/fit.png.meta.json"), "{}").unwrap();
        std::fs::write(run.join("run.json"), "{}").unwrap();
        std::fs::write(run.join("raw_samples.npy"), "SECRET DATA").unwrap();
        std::fs::write(run.join("table.csv"), "1,2").unwrap();
        let art = |kind, path: &str, rendered| Artifact {
            kind,
            path: path.into(),
            size: 1,
            created_at: "t".into(),
            sidecar: None,
            rendered,
        };
        let artifacts = vec![
            art(ArtifactKind::Figure, "out/fit/r1/figures/fit.png", true),
            art(ArtifactKind::Array, "out/fit/r1/raw_samples.npy", false),
            art(ArtifactKind::Table, "out/fit/r1/table.csv", false),
            art(ArtifactKind::Text, "out/fit/r1/missing.txt", true),
        ];
        let paths = rendered_paths(&artifacts, "out/fit/r1");
        assert_eq!(
            paths,
            vec![
                "out/fit/r1/figures/fit.png",
                "out/fit/r1/figures/fit.png.meta.json",
                "out/fit/r1/missing.txt",
                "out/fit/r1/run.json",
            ]
        );
        let local = tempfile::tempdir().unwrap();
        pull_with(
            &sh(pull_remote_script(&mirror.display().to_string())),
            &paths,
            local.path(),
        )
        .await
        .unwrap();
        let got = local.path().join("out/fit/r1");
        assert_eq!(
            std::fs::read_to_string(got.join("figures/fit.png")).unwrap(),
            "PNG"
        );
        assert!(got.join("figures/fit.png.meta.json").is_file());
        assert!(got.join("run.json").is_file());
        assert!(
            !got.join("raw_samples.npy").exists(),
            "data stays on the host"
        );
        assert!(!got.join("table.csv").exists());
        // Nothing to pull is a no-op, and escaping paths are refused.
        pull_with(&sh("exit 1".into()), &[], local.path())
            .await
            .unwrap();
        assert!(
            pull_with(&sh("true".into()), &["../x".into()], local.path())
                .await
                .is_err()
        );
    }

    /// R2: same directory name, different location, different mirror.
    #[test]
    fn target_from_config_and_mirror() {
        let cfg = TargetConfig {
            name: "lab".into(),
            kind: TargetKind::Ssh,
            host: Some("lab-gpu".into()),
            remote_root: Some("~/analyst".into()),
            python: None,
            sandbox: None,
            data_dirs: vec!["/data".into()],
            uv_binary: Some("~/uv/linux-arm64/uv".into()),
        };
        let t = Target::from_config(&cfg).unwrap();
        assert!(matches!(&t, Target::Ssh { uv_binary: Some(p), .. } if p == "~/uv/linux-arm64/uv"));
        let a = t.mirror_dir(Path::new("/home/u/proj")).unwrap();
        let b = t.mirror_dir(Path::new("/home/u/other/proj")).unwrap();
        assert!(a.starts_with("~/analyst/proj-"), "{a}");
        assert_eq!(a.len(), "~/analyst/proj-".len() + 8);
        assert_ne!(a, b);
        assert_eq!(a, t.mirror_dir(Path::new("/home/u/proj")).unwrap());
        let spaced = ssh_target("~/analyst")
            .mirror_dir(Path::new("/home/u/my repo"))
            .unwrap();
        assert!(spaced.starts_with("~/analyst/my_repo-"), "{spaced}");
        assert_eq!(t.host(), Some("lab-gpu"));
        assert!(t.is_remote());
        assert_eq!(Target::Local.mirror_dir(Path::new("/x")), None);
    }

    #[test]
    fn local_bootstrap_state() {
        let home = tempfile::tempdir().unwrap();
        assert!(!bootstrap::local_is_current(home.path()));
        let py = bootstrap::local_tooling_python(home.path());
        assert!(py.starts_with(home.path().join("tooling/.venv")));
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, "").unwrap();
        std::fs::write(home.path().join("tooling/VERSION"), "0.0.0-old\n").unwrap();
        assert!(!bootstrap::local_is_current(home.path()), "version differs");
        std::fs::write(
            home.path().join("tooling/VERSION"),
            format!("{}\n", bootstrap::tooling_version()),
        )
        .unwrap();
        assert!(bootstrap::local_is_current(home.path()));
    }

    /// B12: the tooling environment is created from the kernel source and can import it.
    #[tokio::test]
    async fn ensure_local_installs_the_kernel() {
        let (Some(_uv), Some(src)) = (
            crate::util::find_tool("uv"),
            bootstrap::find_dev_kernel_source(),
        ) else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        let first = match bootstrap::ensure_local(home.path(), &src).await {
            Ok(o) => o,
            Err(e) => {
                // Offline machines cannot resolve the kernel's dependencies.
                eprintln!("skipping: {e:#}");
                return;
            }
        };
        assert!(matches!(first, bootstrap::Outcome::Installed(_)));
        let py = bootstrap::local_tooling_python(home.path());
        let out = run_command(
            &[
                py.to_string_lossy().to_string(),
                "-c".into(),
                "import analyst_kernel".into(),
            ],
            None,
        )
        .await;
        assert!(out.is_ok(), "{out:?}");
        let again = bootstrap::ensure_local(home.path(), &src).await.unwrap();
        assert!(matches!(again, bootstrap::Outcome::UpToDate(_)));
    }
}
