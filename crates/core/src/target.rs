//! Execution targets (ARCHITECTURE 5.5, PROTOCOL 7 and 9): the `Target` enum, local and
//! remote bootstrap, and repo sync (file list, tree hash, rsync or tar push, deletion of
//! extraneous mirror files, pull of rendered artifacts).

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

/// Reject host strings that ssh, rsync, or scp would parse as something else: empty, starting
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
    /// `[A-Za-z0-9._-]` become `_`, which keeps the path safe for rsync and remote shells.
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
                Some(format!(
                    "{}/{}-{}",
                    remote_root.trim_end_matches('/'),
                    base.trim_start_matches('.'),
                    &hash[..8]
                ))
            }
        }
    }
}

/// A `tokio` command for a command line; a bare program name is resolved through
/// [`crate::util::resolve_tool`] so tools are found even when the app was not started from a
/// shell, and children inherit the widened `PATH`.
///
/// Every command line this module runs is built here, so this is where the network gate is
/// consulted ([`crate::egress::guard`]): `ssh`, `scp`, and `rsync` only reach configured SSH
/// targets, and `uv` runs offline unless the line installs from the configured package index.
fn command_for(line: &[String]) -> Result<tokio::process::Command> {
    let (program, args) = line
        .split_first()
        .ok_or_else(|| anyhow!("empty command line"))?;
    let guard = crate::egress::guard(line)?;
    let mut cmd = tokio::process::Command::new(crate::util::resolve_tool(program));
    cmd.args(args)
        .env("PATH", crate::util::tool_path_env())
        .kill_on_drop(true);
    guard.apply_tokio(&mut cmd);
    Ok(cmd)
}

fn check_status(line: &[String], out: &std::process::Output) -> Result<()> {
    if out.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "`{}` failed ({}): {}",
        line.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
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
    let mut cmd = command_for(line)?;
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
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

/// Pipe `producer`'s stdout into `consumer`'s stdin (both run with `cwd`). Used for the tar
/// push and pull pipes.
pub async fn pipe_commands(
    producer: &[String],
    consumer: &[String],
    cwd: Option<&Path>,
) -> Result<()> {
    pipe_commands_with_input(None, producer, consumer, cwd).await
}

/// [`pipe_commands`] with bytes fed to the producer's stdin.
pub async fn pipe_commands_with_input(
    input: Option<Vec<u8>>,
    producer: &[String],
    consumer: &[String],
    cwd: Option<&Path>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut prod = command_for(producer)?;
    prod.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if let Some(d) = cwd {
        prod.current_dir(d);
    }
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
    let mut cons = command_for(consumer)?;
    cons.stdin(Stdio::from(stdout.into_owned_fd()?))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cons.current_dir(d);
    }
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

/// `ssh <opts> -- <host> <remote command>` as a command line.
pub fn ssh_command(host: &str, control_dir: Option<&Path>, remote: &str) -> Vec<String> {
    let mut v = crate::kernel::transport::ssh_prefix(host, control_dir);
    v.push(remote.to_string());
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

    use super::*;
    use analyst_protocol::kernel::Artifact;
    use sha2::{Digest, Sha256};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FileEntry {
        pub path: String,
        pub blob: String,
    }

    fn git_z(repo: &Path, args: &[&str]) -> Result<Vec<Vec<u8>>> {
        let out = crate::checkpoint::git_command(repo, args)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !out.status.success() {
            return Err(anyhow!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| p.to_vec())
            .collect())
    }

    /// Hash working-tree files with `git hash-object --stdin-paths`. Stdin is written from a
    /// second thread while stdout is read, so the pipes can never both fill (B1).
    fn hash_paths(repo: &Path, paths: &[String]) -> Result<Vec<String>> {
        use std::io::Write;
        if paths.is_empty() {
            return Ok(vec![]);
        }
        let mut child = crate::checkpoint::git_command(repo, &["hash-object", "--stdin-paths"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running git hash-object")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("git hash-object stdin not captured"))?;
        let input: Vec<u8> = paths
            .iter()
            .flat_map(|p| format!("{p}\n").into_bytes())
            .collect();
        let writer = std::thread::spawn(move || {
            let r = stdin.write_all(&input);
            drop(stdin);
            r
        });
        let out = child.wait_with_output()?;
        let wrote = writer.join();
        if !out.status.success() {
            return Err(anyhow!(
                "git hash-object failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        if !matches!(wrote, Ok(Ok(()))) {
            return Err(anyhow!("writing paths to git hash-object failed"));
        }
        let shas: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .collect();
        if shas.len() != paths.len() {
            return Err(anyhow!(
                "git hash-object returned {} hashes for {} paths",
                shas.len(),
                paths.len()
            ));
        }
        Ok(shas)
    }

    /// Every file the mirror should hold, with the blob sha of its working-tree content:
    /// tracked files from `git ls-files -s -z` (index shas, no hashing), with modified and
    /// untracked files hashed from disk, and deleted files left out. Sorted by path.
    pub fn file_list(repo: &Path) -> Result<Vec<FileEntry>> {
        use std::collections::{BTreeMap, BTreeSet};
        let mut blobs: BTreeMap<String, String> = BTreeMap::new();
        for rec in git_z(repo, &["ls-files", "-s", "-z"])? {
            // "<mode> <sha> <stage>\t<path>"
            let rec = String::from_utf8_lossy(&rec).to_string();
            let Some((meta, path)) = rec.split_once('\t') else {
                continue;
            };
            let mut it = meta.split(' ');
            let mode = it.next().unwrap_or("");
            let sha = it.next().unwrap_or("");
            if mode == "160000" {
                continue; // submodule link
            }
            blobs.insert(path.to_string(), sha.to_string());
        }
        let lossy = |v: Vec<Vec<u8>>| -> Vec<String> {
            v.into_iter()
                .map(|p| String::from_utf8_lossy(&p).to_string())
                .collect()
        };
        let deleted: BTreeSet<String> = lossy(git_z(repo, &["ls-files", "-d", "-z"])?)
            .into_iter()
            .collect();
        let modified = lossy(git_z(repo, &["ls-files", "-m", "-z"])?);
        let untracked = lossy(git_z(
            repo,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?);
        for d in &deleted {
            blobs.remove(d);
        }
        let mut to_hash: BTreeSet<String> = BTreeSet::new();
        for p in modified.into_iter().chain(untracked) {
            if deleted.contains(&p) {
                continue;
            }
            if p.contains('\n') || !repo.join(&p).is_file() {
                // Unhashable name, a directory (nested repository), or a broken link.
                blobs.remove(&p);
                continue;
            }
            to_hash.insert(p);
        }
        let to_hash: Vec<String> = to_hash.into_iter().collect();
        for (p, sha) in to_hash.iter().zip(hash_paths(repo, &to_hash)?) {
            blobs.insert(p.clone(), sha);
        }
        // A repo's `.env` holds keys for the app, not for code: it never leaves this machine.
        blobs.remove(wb_net::secrets::ENV_FILE);
        Ok(blobs
            .into_iter()
            .filter(|(p, _)| repo.join(p).is_file())
            .map(|(path, blob)| FileEntry { path, blob })
            .collect())
    }

    /// SHA-256 over sorted `(path, blob sha)` pairs, as `"<path>\0<sha>\n"` records.
    pub fn tree_hash(entries: &[FileEntry]) -> String {
        let mut sorted: Vec<&FileEntry> = entries.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        let mut h = Sha256::new();
        for e in sorted {
            h.update(e.path.as_bytes());
            h.update([0u8]);
            h.update(e.blob.as_bytes());
            h.update(b"\n");
        }
        hex::encode(h.finalize())
    }

    /// Write a NUL-separated file list (consumed by `rsync --from0` and `tar --null -T`).
    pub fn write_list_file(entries: &[FileEntry], path: &Path) -> Result<()> {
        let mut buf = Vec::new();
        for e in entries {
            buf.extend_from_slice(e.path.as_bytes());
            buf.push(0);
        }
        std::fs::write(path, buf).with_context(|| format!("writing {}", path.display()))
    }

    /// The ssh client rsync should use, as one `-e` string (same options as every other ssh
    /// call: multiplexing, batch mode, connect timeout, keepalives).
    pub fn rsync_rsh(control_dir: Option<&Path>) -> String {
        let mut parts = vec!["ssh".to_string()];
        parts.extend(
            crate::kernel::transport::ssh_options(control_dir)
                .iter()
                .map(|o| sh_quote(o)),
        );
        parts.join(" ")
    }

    /// Whether a remote path can be handed to rsync as `host:path` without quoting issues on
    /// any rsync version (old versions pass it through the remote shell, new ones do not).
    pub fn rsync_safe_path(path: &str) -> bool {
        !path.is_empty()
            && !path.starts_with('-')
            && path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./~+,@%=".contains(c))
    }

    /// `rsync -az --from0 --files-from=<list> [-e <ssh>] ./ <dest>/`. `--delete` is not used:
    /// with `--files-from` it removes nothing; [`finalize_script`] does the deletion.
    pub fn rsync_args(list_file: &Path, rsh: Option<&str>, dest: &str) -> Vec<String> {
        let mut v = vec![
            "rsync".to_string(),
            "-az".to_string(),
            "--from0".to_string(),
            format!("--files-from={}", list_file.display()),
        ];
        if let Some(rsh) = rsh {
            v.push("-e".into());
            v.push(rsh.to_string());
        }
        v.push("./".into());
        v.push(format!("{}/", dest.trim_end_matches('/')));
        v
    }

    /// The rsync push to `<host>:<mirror>/`.
    pub fn rsync_command(
        host: &str,
        mirror: &str,
        list_file: &Path,
        control_dir: Option<&Path>,
    ) -> Vec<String> {
        // rsync resolves relative remote paths against the home directory, which avoids
        // relying on tilde expansion (not done by rsync versions that escape arguments).
        let remote = match mirror.strip_prefix("~/") {
            Some(rest) => rest.to_string(),
            None => mirror.to_string(),
        };
        rsync_args(
            list_file,
            Some(&rsync_rsh(control_dir)),
            &format!("{host}:{remote}"),
        )
    }

    /// Local half of the tar push: `tar -cf - --null -T <list>` (run in the repo).
    pub fn tar_create_command(list_file: &Path) -> Vec<String> {
        vec![
            "tar".into(),
            "-cf".into(),
            "-".into(),
            "--null".into(),
            "-T".into(),
            list_file.display().to_string(),
        ]
    }

    /// Remote half of the tar push: `mkdir -p <mirror> && tar -xf - -C <mirror>`.
    pub fn tar_extract_remote(mirror: &str) -> String {
        let m = sh_quote_path(mirror);
        format!("mkdir -p {m} && tar -xf - -C {m}")
    }

    /// `ssh <host> '<tar_extract_remote>'` for the tar push consumer.
    pub fn tar_push_ssh_command(
        host: &str,
        mirror: &str,
        control_dir: Option<&Path>,
    ) -> Vec<String> {
        ssh_command(host, control_dir, &tar_extract_remote(mirror))
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

    /// Path of the mirror's recorded tree hash, relative to the mirror.
    pub const TREE_HASH_FILE: &str = ".analyst/.tree-hash";

    /// Remote command run before a push: create the mirror, invalidate its recorded hash (so
    /// an interrupted push never looks complete), and report whether rsync exists.
    pub fn prepare_script(mirror: &str) -> String {
        let m = sh_quote_path(mirror);
        format!(
            "mkdir -p {m}/.analyst && rm -f {m}/{TREE_HASH_FILE}; command -v rsync >/dev/null 2>&1 && echo yes || echo no"
        )
    }

    /// Remote command that prints the mirror's recorded tree hash (empty when absent).
    pub fn read_hash_script(mirror: &str) -> String {
        format!(
            "cat {}/{TREE_HASH_FILE} 2>/dev/null || true",
            sh_quote_path(mirror)
        )
    }

    /// Remote command run after a push. It reads the newline-separated file list on stdin,
    /// deletes every file on the mirror that is not in it (except under `out_dir`, `.venv/`,
    /// `.analyst/`, and `__pycache__/`), removes directories left empty, and records the tree
    /// hash. With `delete` false only the hash is recorded.
    pub fn finalize_script(mirror: &str, out_dir: &str, tree_hash: &str, delete: bool) -> String {
        let m = sh_quote_path(mirror);
        let out = out_dir.trim_matches('/');
        let out_children = sh_quote(&format!("./{out}/*"));
        let out_self = sh_quote(&format!("./{out}"));
        let record = format!("printf '%s\\n' {} > {TREE_HASH_FILE}", sh_quote(tree_hash));
        if !delete {
            return format!("set -e; mkdir -p {m}/.analyst; cd {m}; cat > /dev/null; {record}");
        }
        format!(
            "set -e; mkdir -p {m}/.analyst; cd {m}; cat > .analyst/.sync-list; \
find . \\( -type f -o -type l \\) ! -path {out_children} ! -path './.venv/*' ! -path './.analyst/*' ! -path '*/__pycache__/*' | sed 's|^\\./||' | LC_ALL=C sort > .analyst/.sync-have; \
LC_ALL=C sort .analyst/.sync-list > .analyst/.sync-want; \
LC_ALL=C comm -23 .analyst/.sync-have .analyst/.sync-want | while IFS= read -r f; do rm -f -- \"$f\"; done; \
find . -mindepth 1 -depth -type d -empty ! -path {out_self} ! -path {out_children} ! -path './.venv' ! -path './.venv/*' ! -path './.analyst' ! -path './.analyst/*' -exec rmdir {{}} \\; 2>/dev/null || true; \
rm -f .analyst/.sync-list .analyst/.sync-have .analyst/.sync-want; {record}"
        )
    }

    /// Remote half of a pull: reads NUL-separated mirror-relative paths on stdin and writes a
    /// tar stream of those that exist.
    pub fn pull_remote_script(mirror: &str) -> String {
        format!(
            "cd {} && xargs -0 sh -c 'for f in \"$@\"; do if [ -f \"$f\" ]; then printf \"%s\\0\" \"$f\"; fi; done' sh | tar -cf - --null -T -",
            sh_quote_path(mirror)
        )
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Method {
        Rsync,
        Tar,
        Skipped,
    }

    #[derive(Debug, Clone)]
    pub struct SyncReport {
        pub tree_hash: String,
        pub files: usize,
        pub method: Method,
        pub warning: Option<String>,
    }

    /// Whether `rsync` is installed locally.
    pub fn local_has_rsync() -> bool {
        crate::util::find_tool("rsync").is_some()
    }

    fn out_dir_for(repo: &Path, target: &Target) -> String {
        let project = crate::config::load_project(repo).unwrap_or_default();
        crate::config::effective_project(&project, target.name()).out_dir
    }

    /// Push the local repo to the mirror and delete mirror files that are no longer in the
    /// list. The push is skipped only when the mirror's own recorded tree hash
    /// (`<mirror>/.analyst/.tree-hash`) equals the current one; `last_hash` merely saves the
    /// round trip when the local tree is known to have changed.
    pub async fn push(
        repo: &Path,
        target: &Target,
        control_dir: Option<&Path>,
        last_hash: Option<&str>,
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
        let repo_owned = repo.to_path_buf();
        let entries = tokio::task::spawn_blocking(move || file_list(&repo_owned))
            .await
            .map_err(|e| anyhow!("file list task failed: {e}"))??;
        let hash = tree_hash(&entries);
        let locally_unchanged = last_hash.is_none() || last_hash == Some(hash.as_str());
        if !force && locally_unchanged {
            let remote = run_ssh(host, control_dir, &read_hash_script(&mirror)).await?;
            if remote.trim() == hash {
                return Ok(SyncReport {
                    tree_hash: hash,
                    files: entries.len(),
                    method: Method::Skipped,
                    warning: None,
                });
            }
        }
        let list = tempfile::NamedTempFile::new().context("creating the sync list file")?;
        write_list_file(&entries, list.path())?;
        let remote_has_rsync = run_ssh(host, control_dir, &prepare_script(&mirror))
            .await?
            .trim()
            .ends_with("yes");
        let method = if local_has_rsync() && remote_has_rsync && rsync_safe_path(&mirror) {
            run_command(
                &rsync_command(host, &mirror, list.path(), control_dir),
                Some(repo),
            )
            .await?;
            Method::Rsync
        } else {
            pipe_commands(
                &tar_create_command(list.path()),
                &tar_push_ssh_command(host, &mirror, control_dir),
                Some(repo),
            )
            .await?;
            Method::Tar
        };
        // Deleting needs a newline-separated list; refuse to delete on an empty or
        // unrepresentable one rather than risk wiping the mirror.
        let deletable = !entries.is_empty() && entries.iter().all(|e| !e.path.contains('\n'));
        let mut list_text = String::new();
        for e in &entries {
            list_text.push_str(&e.path);
            list_text.push('\n');
        }
        let script = finalize_script(&mirror, &out_dir_for(repo, target), &hash, deletable);
        run_command_stdin(
            &ssh_command(host, control_dir, &script),
            None,
            Some(list_text.into_bytes()),
        )
        .await
        .context("removing extraneous files from the mirror")?;
        Ok(SyncReport {
            tree_hash: hash,
            files: entries.len(),
            method,
            warning: (!deletable).then(|| {
                "files deleted locally were not removed from the mirror (empty or unusual file list)"
                    .to_string()
            }),
        })
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
    /// into `local_root`, preserving relative paths, with one tar pipe. Replaces the old
    /// whole-directory `pull_run`.
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

    /// The remote command sequence, run after `uv` is in place ([`ensure_remote_uv`]) and
    /// the kernel wheel has been copied to `wheel_remote` (a path on the host). `index_flag`
    /// is the authorized `--default-index '<url>'` from [`crate::egress::remote_index_flag`].
    pub fn commands(
        python_version: &str,
        wheel_remote: &str,
        version: &str,
        index_flag: &str,
    ) -> Vec<String> {
        vec![
            "mkdir -p ~/.analyst/bin ~/.analyst/tooling".to_string(),
            "test -x ~/.analyst/bin/uv".to_string(),
            format!("~/.analyst/bin/uv python install {}", sh_quote(python_version)),
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
    /// [`commands`]).
    pub fn session_env_command(mirror: &str, index_flag: &str) -> String {
        let m = sh_quote_path(mirror);
        format!(
            "cd {m} && if [ -f pyproject.toml ]; then ~/.analyst/bin/uv sync {index_flag}; else ~/.analyst/bin/uv venv {index_flag} && (test -f requirements.txt && ~/.analyst/bin/uv pip install {index_flag} -r requirements.txt || true); fi"
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
        let (host, python) = match target {
            Target::Ssh { host, python, .. } => (host.as_str(), python.as_str()),
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
        let _ = std::fs::remove_file(tooling.join("VERSION"));
        if venv.exists() {
            // Start clean: uv versions differ on whether an existing venv is replaced.
            std::fs::remove_dir_all(&venv)
                .with_context(|| format!("removing {}", venv.display()))?;
        }
        // Both steps may fetch from the package index (the kernel's dependencies), so both
        // name the configured index and are authorized by the gate when spawned.
        run_command(
            &crate::egress::uv_install(
                &uv,
                &["venv"],
                &["-q".to_string(), venv.to_string_lossy().to_string()],
            ),
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

    #[test]
    fn file_list_and_tree_hash() {
        let repo = temp_repo();
        let list = file_list(repo.path()).unwrap();
        let paths: Vec<&str> = list.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec![".gitignore", "scripts/a.py", "untracked.md"]);
        assert!(list.iter().all(|e| e.blob.len() >= 40));
        let h1 = tree_hash(&list);
        assert_eq!(h1.len(), 64);
        // A modified tracked file is hashed from the working tree, not the index.
        std::fs::write(repo.path().join("scripts/a.py"), "print(2)\n").unwrap();
        let l2 = file_list(repo.path()).unwrap();
        let h2 = tree_hash(&l2);
        assert_ne!(h1, h2);
        let direct =
            crate::checkpoint::git(repo.path(), &["hash-object", "scripts/a.py"], None).unwrap();
        assert_eq!(l2[1].blob, direct.trim());
        // A deleted tracked file leaves the list.
        std::fs::remove_file(repo.path().join("scripts/a.py")).unwrap();
        let l3 = file_list(repo.path()).unwrap();
        assert!(l3.iter().all(|e| e.path != "scripts/a.py"));
        // Order independence.
        let mut rev = list.clone();
        rev.reverse();
        assert_eq!(tree_hash(&rev), h1);
    }

    /// B1: with a few thousand paths the old implementation filled both pipes of
    /// `git hash-object` and hung forever.
    #[test]
    fn file_list_does_not_deadlock_on_large_repos() {
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
            .expect("file_list hung")
            .unwrap();
        assert_eq!(n, 6003);
    }

    #[test]
    fn command_lines() {
        let list = PathBuf::from("/tmp/list");
        let ctl = PathBuf::from("/home/u/.analyst/ssh");
        let r = rsync_command("lab", "~/analyst/repo", &list, None);
        assert_eq!(
            r[..4],
            ["rsync", "-az", "--from0", "--files-from=/tmp/list"]
        );
        assert!(!r.contains(&"--delete".to_string()));
        assert_eq!(r[4], "-e");
        assert!(r[5].starts_with("ssh -o BatchMode=yes -o ConnectTimeout=10"));
        assert_eq!(r[6..], ["./", "lab:analyst/repo/"]);
        let r = rsync_command("lab", "/srv/a/repo", &list, Some(&ctl));
        assert!(r[5].contains("ControlPath=/home/u/.analyst/ssh/%C"));
        assert!(r[5].contains("ServerAliveInterval=15"));
        assert_eq!(r.last().unwrap(), "lab:/srv/a/repo/");
        assert!(rsync_safe_path("~/analyst/repo-1a2b3c4d"));
        assert!(!rsync_safe_path("~/my analyst/repo"));
        assert!(!rsync_safe_path("-x"));
        assert_eq!(
            tar_create_command(&list),
            vec!["tar", "-cf", "-", "--null", "-T", "/tmp/list"]
        );
        let push = tar_push_ssh_command("lab", "~/analyst/my repo", None);
        assert_eq!(push[0], "ssh");
        assert_eq!(push[push.len() - 3..push.len() - 1], ["--", "lab"]);
        assert_eq!(
            push.last().unwrap(),
            "mkdir -p ~/'analyst/my repo' && tar -xf - -C ~/'analyst/my repo'"
        );
        assert_eq!(
            tar_extract_local_command(Path::new("/repo")),
            vec!["tar", "-xf", "-", "-C", "/repo"]
        );
        assert!(pull_remote_script("~/a/my repo").starts_with("cd ~/'a/my repo' && xargs -0"));
        assert_eq!(
            read_hash_script("~/a/r"),
            "cat ~/a/r/.analyst/.tree-hash 2>/dev/null || true"
        );
        assert!(prepare_script("~/a/r").starts_with("mkdir -p ~/a/r/.analyst && rm -f"));
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
        assert_eq!(line[line.len() - 3..], ["--", "lab", "true"]);
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
        assert_eq!(cmds[2], "~/.analyst/bin/uv python install 3.12");
        assert!(cmds[3].contains(
            "uv venv --default-index 'https://pypi.org/simple' --python 3.12 ~/.analyst/tooling/.venv"
        ));
        assert!(cmds[4].starts_with(
            "~/.analyst/bin/uv pip install --default-index 'https://pypi.org/simple' --python ~/.analyst/tooling/.venv/bin/python ~/.analyst/tooling/analyst_kernel"
        ));
        assert_eq!(cmds[5], "printf '%s\\n' 0.0.1 > ~/.analyst/tooling/VERSION");
        let script = bootstrap::script("3.12", "w", "v", flag);
        assert!(script.starts_with("set -e; mkdir -p"));
        for text in [script.as_str(), &bootstrap::uv_probe_command()] {
            assert!(
                !text.contains("http://") && !text.contains("curl") && !text.contains("wget"),
                "{text}"
            );
            assert_eq!(text.matches("https://").count(), text.matches(flag).count());
        }
        let env = bootstrap::session_env_command("~/analyst/repo", flag);
        assert!(env.starts_with("cd ~/analyst/repo && "));
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

    /// Populate a stand-in mirror with files that must go and files that must stay.
    fn seed_mirror(mirror: &Path) {
        for (p, text) in [
            ("scripts/deleted_locally.py", "ghost"),
            ("analysis/old/mod.py", "ghost"),
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

    async fn finalize_locally(repo: &Path, mirror: &Path, entries: &[FileEntry]) {
        let list: String = entries.iter().map(|e| format!("{}\n", e.path)).collect();
        let script = finalize_script(&mirror.display().to_string(), "out", "HASH123", true);
        run_command_stdin(&sh(script), Some(repo), Some(list.into_bytes()))
            .await
            .unwrap();
    }

    fn assert_mirror_clean(mirror: &Path) {
        assert_eq!(
            std::fs::read_to_string(mirror.join("scripts/a.py")).unwrap(),
            "print(1)\n"
        );
        assert!(mirror.join("untracked.md").is_file());
        assert!(
            !mirror.join("scripts/deleted_locally.py").exists(),
            "extraneous file must be deleted"
        );
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
        assert!(
            !mirror.join("out/ignored.txt").exists(),
            "ignored files are not synced"
        );
        assert_eq!(
            std::fs::read_to_string(mirror.join(TREE_HASH_FILE)).unwrap(),
            "HASH123\n"
        );
        assert!(!mirror.join(".analyst/.sync-list").exists());
    }

    /// R1: `rsync --delete --files-from` deletes nothing; the finalize step must.
    #[tokio::test]
    async fn rsync_push_then_finalize_deletes_extraneous_files() {
        if !local_has_rsync() {
            return;
        }
        let repo = temp_repo();
        let entries = file_list(repo.path()).unwrap();
        let list = repo.path().join(".list");
        write_list_file(&entries, &list).unwrap();
        let dest = tempfile::tempdir().unwrap();
        let mirror = dest.path().join("mirror");
        seed_mirror(&mirror);
        run_command(
            &rsync_args(&list, None, &mirror.display().to_string()),
            Some(repo.path()),
        )
        .await
        .unwrap();
        assert!(
            mirror.join("scripts/deleted_locally.py").exists(),
            "rsync alone leaves the ghost file"
        );
        finalize_locally(repo.path(), &mirror, &entries).await;
        assert_mirror_clean(&mirror);
    }

    #[tokio::test]
    async fn tar_push_then_finalize_deletes_extraneous_files() {
        let repo = temp_repo();
        let entries = file_list(repo.path()).unwrap();
        let list = repo.path().join(".list");
        write_list_file(&entries, &list).unwrap();
        let dest = tempfile::tempdir().unwrap();
        // A mirror path with a space exercises the quoting of every remote script.
        let mirror = dest.path().join("my mirror");
        seed_mirror(&mirror);
        // Same remote half as the ssh pipe, but run by a local shell.
        pipe_commands(
            &tar_create_command(&list),
            &sh(tar_extract_remote(&mirror.display().to_string())),
            Some(repo.path()),
        )
        .await
        .unwrap();
        finalize_locally(repo.path(), &mirror, &entries).await;
        assert_mirror_clean(&mirror);
        // The recorded hash is readable with the same script the skip check uses.
        let out = run_command(&sh(read_hash_script(&mirror.display().to_string())), None)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "HASH123");
        // Preparing a push invalidates it.
        run_command(&sh(prepare_script(&mirror.display().to_string())), None)
            .await
            .unwrap();
        assert!(!mirror.join(TREE_HASH_FILE).exists());
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
        assert!(rsync_safe_path(&spaced));
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
