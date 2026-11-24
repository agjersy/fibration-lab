//! The core's side of the network gate for subprocesses (ARCHITECTURE 5.6).
//!
//! `wb_net` builds every HTTP request itself, but package installs and SSH run as child
//! processes (`uv`, `ssh`, `scp`, `git push`) that it cannot intercept. Everything in this
//! crate that spawns a command line passes it through [`guard_in`] (or [`guard`]) first, which
//! is the one place those programs are recognised:
//!
//! - `ssh` and `scp` to a remote host are authorized against the gate
//!   ([`wb_net::Gate::authorize_ssh`]): the host must be a configured SSH target, and the
//!   decision is written to the egress ledger. A line whose host cannot be determined is
//!   refused.
//! - `git` may use the network for one thing: the mirror push built by [`git_push`], to an
//!   `ssh://` URL whose host is authorized the same way. The push runs without the user's
//!   git configuration, hooks, or credential helpers. Every other `git` network subcommand
//!   is refused.
//! - `uv` is an install only when the line names the configured package index with
//!   [`INDEX_FLAG`] (lines built by [`uv_install`]); the index is then authorized
//!   ([`wb_net::Gate::authorize_package_index`]), after the project the install runs in has
//!   been checked for package sources of its own ([`sources`]). Every other `uv` invocation
//!   gets [`wb_net::OFFLINE_ENV`], so it fails rather than contact an index.
//! - No `uv` invocation may download a Python interpreter ([`NO_PYTHON_DOWNLOADS`]).
//!
//! `uv` commands that run on a remote host inside an `ssh` line are built with
//! [`remote_index_flag`], which authorizes the index before it hands out the flag; the
//! remote command line itself starts with [`remote_env_prefix`].

pub mod sources;

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock, RwLock};

/// The `uv` option that names the one index an install may use.
pub const INDEX_FLAG: &str = "--default-index";

/// Options that would add a second package source; refused on every `uv` line.
const OTHER_INDEX_FLAGS: &[&str] = &[
    "--index",
    "--index-url",
    "--extra-index-url",
    "--find-links",
];

/// Environment variables through which `uv` or `pip` would pick up another index.
pub const INDEX_ENV_VARS: &[&str] = &[
    "UV_INDEX",
    "UV_DEFAULT_INDEX",
    "UV_INDEX_URL",
    "UV_EXTRA_INDEX_URL",
    "UV_FIND_LINKS",
    "PIP_INDEX_URL",
    "PIP_EXTRA_INDEX_URL",
    "PIP_FIND_LINKS",
];

/// Set on every `uv` the core starts, on this machine and on remote hosts: `uv` must use an
/// interpreter that is already installed and never fetch one.
pub const NO_PYTHON_DOWNLOADS: (&str, &str) = ("UV_PYTHON_DOWNLOADS", "never");

/// The ref the mirror push writes on the host. Nothing outside `refs/fibration/` is pushed.
pub const SYNC_REF: &str = "refs/fibration/sync";

/// Git settings for a process on a remote host: no system or user configuration, so no
/// hooks path, filter, or helper configured there can run.
pub const REMOTE_GIT_ENV: &str = "GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1";

fn index_cell() -> &'static RwLock<String> {
    static CELL: OnceLock<RwLock<String>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(wb_net::DEFAULT_PACKAGE_INDEX.to_string()))
}

/// Record the package index from the user's configuration (`[packages] index_url`) and put
/// it on the gate's allowlist. Called when the global config is loaded.
pub fn set_package_index(url: &str) -> Result<()> {
    let url = url.trim();
    wb_net::gate()
        .allow_package_index(url)
        .map_err(|e| anyhow!("{e}"))?;
    if let Ok(mut w) = index_cell().write() {
        *w = url.to_string();
    }
    Ok(())
}

/// The configured package index (`https://pypi.org/simple` unless configured otherwise).
pub fn package_index() -> String {
    index_cell()
        .read()
        .map(|s| s.clone())
        .unwrap_or_else(|_| wb_net::DEFAULT_PACKAGE_INDEX.to_string())
}

/// A refusal by the gate for a subprocess, in words a user can act on.
fn refusal(e: &wb_net::EgressError) -> anyhow::Error {
    use wb_net::EgressError::*;
    match e {
        Denied {
            class, destination, ..
        } if *class == "ssh" => anyhow!(
            "not connecting to `{destination}`: it is not the host of an SSH target in config.toml. The app only opens SSH connections to configured targets"
        ),
        Denied { destination, .. } => anyhow!(
            "not installing from {destination}: it is not the package index configured as `[packages] index_url` in config.toml"
        ),
        BadDestination(d) => anyhow!("`{d}` is not a usable network destination"),
        Ledger(why) => anyhow!(
            "the network ledger (egress.jsonl in the fibration home) could not be written ({why}); nothing leaves the machine unrecorded, so the command was not run"
        ),
        Client(why) => anyhow!("{why}"),
    }
}

/// A `uv` install command line: `<uv> <subcommand...> --default-index <configured index>
/// <rest...>`. [`guard`] authorizes the index when the line is spawned.
pub fn uv_install<S: AsRef<str>>(uv: &str, subcommand: &[&str], rest: &[S]) -> Vec<String> {
    let mut line = vec![uv.to_string()];
    line.extend(subcommand.iter().map(|s| s.to_string()));
    line.push(INDEX_FLAG.to_string());
    line.push(package_index());
    line.extend(rest.iter().map(|s| s.as_ref().to_string()));
    line
}

/// For a `uv` install that runs on a remote host: authorize the configured index (recorded
/// in the ledger under `purpose`) and return `--default-index '<url>'`, shell-quoted, to
/// place in the remote command.
pub fn remote_index_flag(purpose: &str) -> Result<String> {
    let index = package_index();
    wb_net::gate()
        .authorize_package_index(&index, purpose)
        .map_err(|e| refusal(&e))?;
    Ok(format!("{INDEX_FLAG} {}", crate::util::sh_quote(&index)))
}

/// Shell text placed in front of every command the core runs on a remote host
/// (`target::ssh_command`): any `uv` started there inherits [`NO_PYTHON_DOWNLOADS`].
pub fn remote_env_prefix() -> String {
    format!(
        "export {}={}; ",
        NO_PYTHON_DOWNLOADS.0, NO_PYTHON_DOWNLOADS.1
    )
}

/// What to tell the user when `uv` has no interpreter to use. `request` is the version or
/// specifier that was asked for (`3.12`, `>=3.11`); `machine` names where to install it.
pub fn python_missing(request: &str, machine: &str) -> String {
    let wanted = if request.is_empty() {
        "Python".to_string()
    } else {
        format!("Python {request}")
    };
    format!(
        "no {wanted} interpreter was found on {machine}. The app never downloads Python interpreters. Install {wanted} on {machine} yourself (its package manager is the usual way), then try again"
    )
}

/// If `stderr` is `uv` reporting that it found no interpreter, the message for the user
/// ([`python_missing`]) with the version `uv` was looking for.
pub fn explain_missing_python(stderr: &str, machine: &str) -> Option<String> {
    let at = stderr.find("No interpreter found")?;
    let rest = &stderr[at + "No interpreter found".len()..];
    let line = rest.lines().next().unwrap_or("");
    let request = line
        .strip_prefix(" for ")
        .and_then(|r| r.split(" in ").next())
        .map(|r| r.trim().trim_start_matches("Python").trim())
        .unwrap_or("");
    Some(python_missing(request, machine))
}

/// Check that `uv` can find an installed interpreter before an environment is created or
/// synced: `request` (a version or specifier) when given, else what the project in `cwd`
/// requires. Fails with [`python_missing`]. Nothing is downloaded.
pub fn require_python(cwd: &Path, request: Option<&str>) -> Result<()> {
    let mut line = vec![
        "uv".to_string(),
        "python".into(),
        "find".into(),
        "--system".into(),
    ];
    line.extend(request.map(str::to_string));
    let guard = guard(&line)?;
    let mut cmd = std::process::Command::new(crate::util::resolve_tool("uv"));
    cmd.args(&line[1..])
        .current_dir(cwd)
        .env("PATH", crate::util::tool_path_env())
        .stdin(std::process::Stdio::null());
    guard.apply_std(&mut cmd);
    let Ok(out) = cmd.output() else {
        return Ok(()); // no uv: the install itself reports that
    };
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    match explain_missing_python(&stderr, "this machine") {
        Some(message) => Err(anyhow!(message)),
        // Anything else (an invalid project file, say) is for the install to report.
        None => Ok(()),
    }
}

/// Fail when the project in `root` names package sources other than the configured index
/// (see [`sources`]). [`guard_in`] runs this for every install line; call it directly for a
/// line that is spawned by code that does not pass the working directory to the guard.
pub fn check_project_sources(root: &Path) -> Result<()> {
    sources::check(root, &package_index())
}

fn mirrors() -> &'static Mutex<HashMap<String, std::path::PathBuf>> {
    static CELL: OnceLock<Mutex<HashMap<String, std::path::PathBuf>>> = OnceLock::new();
    CELL.get_or_init(Mutex::default)
}

/// Remember that `mirror` (a directory on a remote host) holds a copy of the local project
/// `repo`, so an install that runs in the mirror is checked against the project's files
/// ([`check_project_sources`]). Called whenever a mirror path is computed.
pub fn note_mirror(mirror: &str, repo: &Path) {
    if let Ok(mut m) = mirrors().lock() {
        if m.get(mirror).map(|p| p.as_path()) != Some(repo) {
            m.insert(mirror.to_string(), repo.to_path_buf());
        }
    }
}

/// The local projects whose mirror directory a remote command names.
fn projects_named_in(remote: &str) -> Vec<std::path::PathBuf> {
    mirrors()
        .lock()
        .map(|m| {
            m.iter()
                .filter(|(mirror, _)| remote.contains(&crate::util::sh_quote_path(mirror)))
                .map(|(_, repo)| repo.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The ssh client as one shell-quoted string, with the options every ssh call carries
/// (multiplexing, batch mode, connect timeout, keepalives). Git runs it for a push.
pub fn ssh_shell_command(control_dir: Option<&Path>) -> String {
    let mut parts = vec!["ssh".to_string()];
    parts.extend(
        crate::kernel::transport::ssh_options(control_dir)
            .iter()
            .map(|o| crate::util::sh_quote(o)),
    );
    parts.join(" ")
}

/// `ssh://<host>/<absolute path>` with the path percent-encoded (git decodes it).
fn ssh_url(host: &str, abs_path: &str) -> Result<String> {
    if !abs_path.starts_with('/') {
        return Err(anyhow!("`{abs_path}` is not an absolute path on the host"));
    }
    if host.contains(['/', '%', '?', '#', '[', ']']) || host.contains(':') {
        return Err(anyhow!(
            "ssh host `{host}` cannot be used in a git URL; give the host a plain name (an alias in ~/.ssh/config can carry a port or an IPv6 address)"
        ));
    }
    let mut url = format!("ssh://{host}");
    for b in abs_path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(url)
}

/// The mirror push: `git push` of `commit` to [`SYNC_REF`] in the bare repository at
/// `remote_repo` (an absolute path on `host`), run from `git_dir`.
///
/// Nothing configurable by a repository or a user decides what this command runs: hooks are
/// off on both sides, there is no credential helper, the ssh client is the one every other
/// call uses, and [`guard`] removes the user's and the system's git configuration from the
/// child (so no `url.*.insteadOf` can redirect it). `git_dir` should be a scratch repository
/// that borrows the project's objects, so the project's own `.git/config` is not read
/// either.
pub fn git_push(
    git_dir: &Path,
    host: &str,
    remote_repo: &str,
    commit: &str,
    control_dir: Option<&Path>,
) -> Result<Vec<String>> {
    let mut line = vec!["git".to_string()];
    line.extend(
        crate::checkpoint::GIT_HARDENING
            .iter()
            .map(|s| s.to_string()),
    );
    for setting in [
        "credential.helper=".to_string(),
        format!("core.sshCommand={}", ssh_shell_command(control_dir)),
    ] {
        line.push("-c".into());
        line.push(setting);
    }
    line.extend([
        "--git-dir".to_string(),
        git_dir.display().to_string(),
        "push".into(),
        "--quiet".into(),
        "--force".into(),
        "--no-verify".into(),
        "--recurse-submodules=no".into(),
        format!("--receive-pack=env {REMOTE_GIT_ENV} git -c core.hooksPath=/dev/null receive-pack"),
        "--".into(),
        ssh_url(host, remote_repo)?,
        format!("{commit}:{SYNC_REF}"),
    ]);
    Ok(line)
}

/// What [`guard`] decided for a command line: changes to make to the child's environment.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Guard {
    /// Set when the command must not use the network ([`wb_net::OFFLINE_ENV`]).
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    /// Set whenever the program runs, networked or not: [`NO_PYTHON_DOWNLOADS`] for `uv`,
    /// the configuration lockout for a git push.
    pub tool_env: Vec<(String, String)>,
}

impl Guard {
    pub fn apply_tokio(&self, cmd: &mut tokio::process::Command) {
        for name in &self.env_remove {
            cmd.env_remove(name);
        }
        for (k, v) in self.env.iter().chain(&self.tool_env) {
            cmd.env(k, v);
        }
    }

    pub fn apply_std(&self, cmd: &mut std::process::Command) {
        for name in &self.env_remove {
            cmd.env_remove(name);
        }
        for (k, v) in self.env.iter().chain(&self.tool_env) {
            cmd.env(k, v);
        }
    }
}

fn program_name(program: &str) -> String {
    let name = Path::new(program)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| program.to_string());
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// The host of `host:path` (an scp remote operand); `None` for a local path.
fn remote_operand_host(arg: &str) -> Option<&str> {
    let (host, _) = arg.split_once(':')?;
    // A colon after a slash, or a leading `.`/`/`, is part of a local path.
    if host.is_empty() || host.contains('/') || host.starts_with('.') {
        return None;
    }
    Some(host)
}

/// The remote hosts an `ssh` or `scp` line would connect to. An error means the line cannot
/// be understood, and is therefore not run.
fn ssh_hosts(name: &str, args: &[String]) -> Result<Vec<String>> {
    if name == "ssh" {
        // Lines are built by `ssh_prefix`: options, `--`, then the host.
        let at = args
            .iter()
            .position(|a| a == "--")
            .ok_or_else(|| anyhow!("refusing an ssh command line without `--` before the host"))?;
        let host = args
            .get(at + 1)
            .ok_or_else(|| anyhow!("the ssh command line names no host"))?;
        return Ok(vec![host.clone()]);
    }
    let at = args
        .iter()
        .position(|a| a == "--")
        .ok_or_else(|| anyhow!("refusing an scp command line without `--` before the paths"))?;
    let hosts: Vec<String> = args[at + 1..]
        .iter()
        .filter_map(|a| remote_operand_host(a))
        .map(str::to_string)
        .collect();
    if hosts.is_empty() {
        return Err(anyhow!("the scp command line names no remote host"));
    }
    Ok(hosts)
}

/// Git subcommands that open a connection.
const GIT_NETWORK_SUBCOMMANDS: &[&str] = &[
    "push",
    "fetch",
    "pull",
    "clone",
    "ls-remote",
    "send-pack",
    "fetch-pack",
    "remote",
    "submodule",
    "archive",
    "bundle",
    "lfs",
];

/// Environment variables through which a caller's environment could change what a git push
/// runs or where it connects.
const GIT_ENV_REMOVE: &[&str] = &[
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_PROXY_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_EXEC_PATH",
    "GIT_TRACE",
];

/// A `git` line: local subcommands pass; the only network use is a push in the form
/// [`git_push`] builds, to a configured SSH target and a ref under `refs/fibration/`.
fn guard_git(args: &[String]) -> Result<Guard> {
    // The subcommand is the first word that is neither an option nor an option's value.
    let mut it = args.iter().enumerate();
    let mut sub = None;
    while let Some((i, a)) = it.next() {
        if matches!(
            a.as_str(),
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            it.next();
        } else if !a.starts_with('-') {
            sub = Some((i, a.as_str()));
            break;
        }
    }
    let Some((at, sub)) = sub else {
        return Ok(Guard::default());
    };
    if !GIT_NETWORK_SUBCOMMANDS.contains(&sub) {
        return Ok(Guard::default());
    }
    let refuse = |why: &str| {
        anyhow!(
            "refusing `git {sub}`: {why}. The app uses git over the network only to push the working tree to the mirror of a configured SSH target"
        )
    };
    if sub != "push" {
        return Err(refuse("it could contact a remote"));
    }
    let rest = &args[at + 1..];
    let operands = match rest.iter().position(|a| a == "--") {
        Some(i) => &rest[i + 1..],
        None => return Err(refuse("the line has no `--` before the destination")),
    };
    let [url, refspec] = operands else {
        return Err(refuse("it must name one URL and one refspec"));
    };
    let authority = url
        .strip_prefix("ssh://")
        .and_then(|r| r.split('/').next())
        .filter(|a| !a.is_empty() && !a.contains(['%', ':', '[']))
        .ok_or_else(|| refuse("the destination is not a plain `ssh://host/path` URL"))?;
    if !refspec
        .split_once(':')
        .is_some_and(|(_, dst)| dst.starts_with("refs/fibration/"))
    {
        return Err(refuse("it may only write refs under refs/fibration/"));
    }
    wb_net::gate()
        .authorize_ssh(authority, "git push")
        .map_err(|e| refusal(&e))?;
    let tool_env: Vec<(String, String)> = [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_COUNT", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    Ok(Guard {
        env: vec![],
        env_remove: GIT_ENV_REMOVE.iter().map(|s| s.to_string()).collect(),
        tool_env,
    })
}

/// The value of [`INDEX_FLAG`] on a `uv` line, if present (before any `--`).
fn index_of(args: &[String]) -> Result<Option<String>> {
    let mut found = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        let flag = a.split('=').next().unwrap_or(a);
        if OTHER_INDEX_FLAGS.contains(&flag) {
            return Err(anyhow!(
                "refusing `uv` with `{flag}`: installs use the one configured package index"
            ));
        }
        if a == INDEX_FLAG {
            found = Some(
                it.next()
                    .ok_or_else(|| anyhow!("`{INDEX_FLAG}` without a URL"))?
                    .clone(),
            );
        } else if let Some(v) = a.strip_prefix("--default-index=") {
            found = Some(v.to_string());
        }
    }
    Ok(found)
}

/// [`guard_in`] for a command line whose working directory is not known here. An install
/// line is authorized but its project is not read: the caller runs
/// [`check_project_sources`] itself.
pub fn guard(line: &[String]) -> Result<Guard> {
    guard_in(line, None)
}

/// Check a command line before it is spawned in `cwd` and say how its environment must
/// change. See the module documentation. Lines that run none of `uv`, an SSH client, or a
/// git network subcommand pass unchanged.
pub fn guard_in(line: &[String], cwd: Option<&Path>) -> Result<Guard> {
    let Some((program, args)) = line.split_first() else {
        return Ok(Guard::default());
    };
    let name = program_name(program);
    match name.as_str() {
        "rsync" | "sftp" => Err(anyhow!("{name} is not used by the app and is not allowed")),
        "ssh" | "scp" => {
            // An install that runs in a mirror installs the local project's dependencies.
            if let Some(remote) = args
                .last()
                .filter(|r| name == "ssh" && r.contains(INDEX_FLAG))
            {
                for project in projects_named_in(remote) {
                    check_project_sources(&project)?;
                }
            }
            for host in ssh_hosts(&name, args)? {
                wb_net::gate()
                    .authorize_ssh(&host, &name)
                    .map_err(|e| refusal(&e))?;
            }
            Ok(Guard::default())
        }
        "git" => guard_git(args),
        "uv" | "uvx" | "pip" | "pip3" => {
            let env_remove: Vec<String> = INDEX_ENV_VARS.iter().map(|s| s.to_string()).collect();
            let tool_env = vec![(
                NO_PYTHON_DOWNLOADS.0.to_string(),
                NO_PYTHON_DOWNLOADS.1.to_string(),
            )];
            match index_of(args)? {
                Some(index) => {
                    if let Some(dir) = cwd {
                        check_project_sources(dir)?;
                    }
                    let purpose = format!(
                        "{name} {}",
                        args.iter()
                            .take_while(|a| !a.starts_with('-'))
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    wb_net::gate()
                        .authorize_package_index(&index, purpose.trim())
                        .map_err(|e| refusal(&e))?;
                    Ok(Guard {
                        env: vec![],
                        env_remove,
                        tool_env,
                    })
                }
                None => Ok(Guard {
                    env: wb_net::OFFLINE_ENV
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    env_remove,
                    tool_env,
                }),
            }
        }
        _ => Ok(Guard::default()),
    }
}

// ---- no-network wrapping on remote hosts ----

/// `bwrap <wb_sandbox::no_network::BWRAP_NO_NETWORK_ARGS> -- ` as a shell prefix for a
/// command that runs on a remote Linux host.
pub fn remote_bwrap_prefix() -> String {
    let mut parts = vec!["bwrap".to_string()];
    parts.extend(
        wb_sandbox::no_network::BWRAP_NO_NETWORK_ARGS
            .iter()
            .map(|a| a.to_string()),
    );
    parts.push("--".into());
    parts.join(" ") + " "
}

/// The remote command that reports whether `bwrap` can take the network away there.
pub fn remote_bwrap_probe_command() -> String {
    format!(
        "{}true >/dev/null 2>&1 && echo fibration-bwrap-yes || echo fibration-bwrap-no",
        remote_bwrap_prefix()
    )
}

fn remote_probe_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// Whether `bwrap --unshare-net` works on `host`, asked over SSH once per host and process.
/// A failed connection is reported as an error and not remembered.
pub async fn remote_bwrap_works(host: &str, control_dir: Option<&Path>) -> Result<bool> {
    if let Some(known) = remote_probe_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(host).copied())
    {
        return Ok(known);
    }
    let out = crate::target::run_ssh(host, control_dir, &remote_bwrap_probe_command()).await?;
    let works = out.contains("fibration-bwrap-yes");
    if let Ok(mut c) = remote_probe_cache().lock() {
        c.insert(host.to_string(), works);
    }
    Ok(works)
}

/// The label reported for a remote process: `bwrap` when wrapped, else `none`.
pub fn remote_isolation_label(wrapped: bool) -> &'static str {
    if wrapped {
        "bwrap"
    } else {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ssh_clients_need_a_configured_host() {
        let tag = crate::util::short_rand().to_lowercase();
        let host = format!("lab-{tag}");
        let ssh = crate::target::ssh_command(&host, None, "true");
        let err = guard(&ssh).unwrap_err().to_string();
        assert!(err.contains(&host) && err.contains("config.toml"), "{err}");
        let scp = line(&[
            "scp",
            "-q",
            "--",
            "/tmp/uv",
            &format!("me@{host}:.fibration/bin/uv"),
        ]);
        assert!(guard(&scp).is_err());
        let push = git_push(
            Path::new("/tmp/scratch"),
            &host,
            "/home/u/m.git",
            "abc",
            None,
        )
        .unwrap();
        assert!(guard(&push).is_err());

        wb_net::gate().allow_ssh_host(&host).unwrap();
        assert_eq!(guard(&ssh).unwrap(), Guard::default());
        assert!(guard(&scp).is_ok());
        assert!(guard(&push).is_ok());
        // A resolved path to the client is recognised too.
        let mut abs = ssh.clone();
        abs[0] = "/usr/bin/ssh".into();
        assert!(guard(&abs).is_ok());
        let host_at = abs.len() - 2;
        abs[host_at] = format!("other-{tag}");
        assert!(guard(&abs).is_err());

        // Lines that cannot be understood are refused, and so are clients the app has no
        // use for.
        assert!(guard(&line(&["ssh", &host, "true"])).is_err());
        assert!(guard(&line(&["scp", "--", "/a", "/b"])).is_err());
        assert!(guard(&line(&["sftp", &host])).is_err());
        assert!(guard(&line(&["rsync", "-az", "./", &format!("{host}:x/")])).is_err());
        assert_eq!(guard(&line(&["git", "status"])).unwrap(), Guard::default());
    }

    /// The mirror push: one form, one destination class, and none of the user's git
    /// configuration.
    #[test]
    fn git_reaches_the_network_only_to_push_to_a_configured_target() {
        let tag = crate::util::short_rand().to_lowercase();
        let host = format!("push-{tag}");
        let ctl = Path::new("/home/u/.fibration/ssh dir");
        let push = git_push(
            Path::new("/tmp/scratch"),
            &format!("me@{host}"),
            "/home/u/my fibration/repo-1a2b3c4d.git",
            "0123abcd",
            Some(ctl),
        )
        .unwrap();
        assert_eq!(push[0], "git");
        for setting in [
            "core.hooksPath=/dev/null",
            "credential.helper=",
            "core.fsmonitor=",
        ] {
            let at = push.iter().position(|a| a == setting).expect(setting);
            assert_eq!(push[at - 1], "-c");
        }
        let ssh = push
            .iter()
            .find_map(|a| a.strip_prefix("core.sshCommand="))
            .unwrap();
        assert!(ssh.starts_with("ssh -o ControlMaster=auto -o "), "{ssh}");
        assert!(
            ssh.contains("'ControlPath=/home/u/.fibration/ssh dir/%C'"),
            "{ssh}"
        );
        assert!(
            ssh.contains("-o BatchMode=yes -o ConnectTimeout=10"),
            "{ssh}"
        );
        assert!(push.contains(&"--no-verify".to_string()));
        assert!(push.contains(
            &"--receive-pack=env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null receive-pack"
                .to_string()
        ));
        assert_eq!(
            push[push.len() - 3..],
            [
                "--".to_string(),
                format!("ssh://me@{host}/home/u/my%20fibration/repo-1a2b3c4d.git"),
                "0123abcd:refs/fibration/sync".to_string()
            ]
        );
        // Hosts that would change what the URL means are refused before anything runs.
        for bad in ["lab:2222", "lab/x", "lab%2e", "[::1]"] {
            assert!(
                git_push(Path::new("/s"), bad, "/r.git", "abc", None).is_err(),
                "{bad}"
            );
        }
        assert!(git_push(Path::new("/s"), &host, "~/r.git", "abc", None).is_err());

        // Not a configured target: refused, with the host named.
        let err = guard(&push).unwrap_err().to_string();
        assert!(err.contains(&host) && err.contains("config.toml"), "{err}");
        wb_net::gate().allow_ssh_host(&host).unwrap();
        let g = guard(&push).unwrap();
        for pair in [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_COUNT", "0"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ] {
            assert!(
                g.tool_env
                    .contains(&(pair.0.to_string(), pair.1.to_string())),
                "{pair:?}"
            );
        }
        for name in [
            "GIT_SSH_COMMAND",
            "GIT_SSH",
            "GIT_ASKPASS",
            "GIT_CONFIG_PARAMETERS",
        ] {
            assert!(g.env_remove.iter().any(|n| n == name), "{name}");
        }
        // The same push to another host, to a branch, or without the fixed form is refused.
        let url_at = push.len() - 2;
        let mut other = push.clone();
        other[url_at] = format!("ssh://other-{tag}/home/u/r.git");
        assert!(guard(&other).is_err());
        let mut branch = push.clone();
        branch[url_at + 1] = "0123abcd:refs/heads/main".into();
        let err = guard(&branch).unwrap_err().to_string();
        assert!(err.contains("refs/fibration/"), "{err}");
        for l in [
            line(&[
                "git",
                "push",
                "--",
                &format!("{host}:r.git"),
                "a:refs/fibration/sync",
            ]),
            line(&[
                "git",
                "push",
                "--",
                "https://example.org/r.git",
                "a:refs/fibration/sync",
            ]),
            line(&[
                "git",
                "push",
                &format!("ssh://{host}/r.git"),
                "a:refs/fibration/sync",
            ]),
            line(&["git", "push", "--", &format!("ssh://{host}/r.git")]),
            line(&["git", "-C", "/repo", "fetch", "origin"]),
            line(&["git", "-c", "x=y", "clone", &format!("ssh://{host}/r.git")]),
            line(&["/usr/bin/git", "ls-remote", "origin"]),
        ] {
            assert!(guard(&l).is_err(), "{l:?}");
        }
        // Local git work is not the gate's business.
        for l in [
            line(&["git", "-c", "core.hooksPath=/dev/null", "status"]),
            line(&["git", "-C", "push", "log"]),
            line(&["git", "--git-dir", "fetch", "rev-parse", "HEAD"]),
        ] {
            assert_eq!(guard(&l).unwrap(), Guard::default(), "{l:?}");
        }
    }

    /// No `uv` the core starts may download a Python interpreter, and a missing one is
    /// reported with the version to install and where.
    #[test]
    fn uv_never_downloads_python() {
        wb_net::gate()
            .allow_package_index(&package_index())
            .unwrap();
        let never = ("UV_PYTHON_DOWNLOADS".to_string(), "never".to_string());
        for l in [
            line(&["uv", "sync"]),
            line(&["uv", "python", "install", "3.12"]),
            line(&["uv", "venv", "--python", "3.12"]),
            line(&["/opt/bin/uv", "pip", "list"]),
            uv_install("uv", &["sync"], &["-q"]),
            uv_install("uv", &["venv"], &["-q"]),
            uv_install("uv", &["add"], &["--", "numpy"]),
            uv_install("uv", &["pip", "install"], &["--", "numpy"]),
            uv_install("uv", &["build"], &["--wheel"]),
        ] {
            let g = guard(&l).unwrap();
            assert!(g.tool_env.contains(&never), "{l:?}");
            // The setting reaches the child whichever way the guard is applied.
            let mut cmd = std::process::Command::new("true");
            g.apply_std(&mut cmd);
            assert!(
                cmd.get_envs()
                    .any(|(k, v)| k == "UV_PYTHON_DOWNLOADS" && v.is_some_and(|v| v == "never")),
                "{l:?}"
            );
            let mut cmd = tokio::process::Command::new("true");
            g.apply_tokio(&mut cmd);
            assert!(cmd
                .as_std()
                .get_envs()
                .any(|(k, v)| k == "UV_PYTHON_DOWNLOADS" && v.is_some_and(|v| v == "never")));
        }
        // Remote hosts get the same setting in front of every command.
        assert_eq!(remote_env_prefix(), "export UV_PYTHON_DOWNLOADS=never; ");
        assert!(
            crate::target::ssh_command("lab", None, "~/.fibration/bin/uv sync")
                .last()
                .unwrap()
                .starts_with("export UV_PYTHON_DOWNLOADS=never; ")
        );

        // uv's own report becomes a message that names the version and the machine.
        let uv = "warning: something\nerror: No interpreter found for Python >=3.12 in managed installations or search path\n";
        let msg = explain_missing_python(uv, "this machine").unwrap();
        assert!(
            msg.contains("no Python >=3.12 interpreter was found on this machine")
                && msg.contains("never downloads Python")
                && msg.contains("Install Python >=3.12 on this machine"),
            "{msg}"
        );
        let msg = explain_missing_python(
            "error: No interpreter found for Python 3.7 in virtual environments, managed installations, or search path",
            "lab-gpu",
        )
        .unwrap();
        assert!(
            msg.contains("Python 3.7") && msg.contains("on lab-gpu"),
            "{msg}"
        );
        let msg =
            explain_missing_python("error: No interpreter found in search path", "x").unwrap();
        assert!(
            msg.starts_with("no Python interpreter was found on x"),
            "{msg}"
        );
        assert_eq!(
            explain_missing_python("error: resolution failed", "x"),
            None
        );
        assert!(!python_missing("3.12", "lab").contains("http"));
    }

    /// With real `uv`: a version nobody has installed is refused before any environment is
    /// touched, and nothing is fetched to satisfy it.
    #[test]
    fn a_missing_interpreter_is_reported_before_installing() {
        if crate::util::find_tool("uv").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        require_python(dir.path(), None).unwrap();
        let err = require_python(dir.path(), Some("3.99"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no Python 3.99 interpreter was found on this machine")
                && err.contains("Install Python 3.99"),
            "{err}"
        );
        // A project that asks for it is refused the same way, by its own requirement.
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0\"\nrequires-python = \">=3.99\"\n",
        )
        .unwrap();
        let err = require_python(dir.path(), None).unwrap_err().to_string();
        assert!(err.contains("Python >=3.99"), "{err}");
    }

    /// Installs are refused for a project that names its own package sources: by the guard
    /// when it knows the directory, and for a remote install by the mirror the command
    /// names.
    #[test]
    fn installs_read_the_project_first() {
        wb_net::gate()
            .allow_package_index(&package_index())
            .unwrap();
        let clean = tempfile::tempdir().unwrap();
        std::fs::write(
            clean.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0\"\ndependencies = [\"numpy\"]\n",
        )
        .unwrap();
        let dirty = tempfile::tempdir().unwrap();
        std::fs::write(
            dirty.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0\"\ndependencies = [\"helper\"]\n\n[tool.uv.sources]\nhelper = { git = \"https://github.com/someone/helper\" }\n",
        )
        .unwrap();
        for sub in [
            &["sync"][..],
            &["add"],
            &["remove"],
            &["lock"],
            &["pip", "install"],
        ] {
            let l = uv_install("uv", sub, &["-q"]);
            assert!(guard_in(&l, Some(clean.path())).is_ok(), "{l:?}");
            let err = guard_in(&l, Some(dirty.path())).unwrap_err().to_string();
            assert!(
                err.contains("pyproject.toml") && err.contains("tool.uv.sources.helper"),
                "{err}"
            );
        }
        assert!(check_project_sources(clean.path()).is_ok());
        assert!(check_project_sources(dirty.path()).is_err());
        // Offline uv work in the same project is not an install and is not refused.
        assert!(guard_in(&line(&["uv", "pip", "list"]), Some(dirty.path())).is_ok());

        // Remote: the install runs in the mirror of the local project.
        let tag = crate::util::short_rand().to_lowercase();
        let host = format!("mirror-{tag}");
        wb_net::gate().allow_ssh_host(&host).unwrap();
        let flag = remote_index_flag("uv sync (remote)").unwrap();
        let (m_clean, m_dirty) = (
            format!("~/fibration/clean-{tag}"),
            format!("~/fibration/dirty-{tag}"),
        );
        note_mirror(&m_clean, clean.path());
        note_mirror(&m_dirty, dirty.path());
        let remote = |mirror: &str| {
            crate::target::ssh_command(
                &host,
                None,
                &crate::target::bootstrap::session_env_command(mirror, &flag),
            )
        };
        assert!(guard(&remote(&m_clean)).is_ok());
        let err = guard(&remote(&m_dirty)).unwrap_err().to_string();
        assert!(err.contains("tool.uv.sources.helper"), "{err}");
        // Other commands in that mirror are not installs.
        assert!(guard(&crate::target::ssh_command(
            &host,
            None,
            &format!("cd {m_dirty} && ls")
        ))
        .is_ok());
    }

    #[test]
    fn uv_is_offline_unless_it_installs_from_the_configured_index() {
        // No index flag: offline, whatever the subcommand.
        for l in [
            line(&["uv", "sync"]),
            line(&["/opt/bin/uv", "add", "--", "numpy"]),
            line(&["uv", "pip", "list", "--format", "json"]),
            line(&["uv", "build", "--wheel"]),
        ] {
            let g = guard(&l).unwrap();
            assert!(
                g.env.contains(&("UV_OFFLINE".to_string(), "1".to_string())),
                "{l:?}"
            );
            assert!(g.env_remove.iter().any(|n| n == "UV_EXTRA_INDEX_URL"));
        }
        // The helper names the configured index, and the gate lets that one through.
        wb_net::gate()
            .allow_package_index(&package_index())
            .unwrap();
        let add = uv_install("uv", &["add"], &["--", "numpy"]);
        assert_eq!(
            add,
            line(&["uv", "add", INDEX_FLAG, &package_index(), "--", "numpy"])
        );
        let g = guard(&add).unwrap();
        assert!(g.env.is_empty(), "an install is not forced offline");
        assert!(g.tool_env.contains(&(
            NO_PYTHON_DOWNLOADS.0.to_string(),
            NO_PYTHON_DOWNLOADS.1.to_string()
        )));
        assert!(g.env_remove.iter().any(|n| n == "UV_INDEX_URL"));
        // Any other index is refused, as is a second package source.
        let elsewhere = line(&[
            "uv",
            "add",
            INDEX_FLAG,
            "https://packages.elsewhere.example/simple",
            "--",
            "numpy",
        ]);
        let err = guard(&elsewhere).unwrap_err().to_string();
        assert!(err.contains("packages.elsewhere.example"), "{err}");
        assert!(guard(&line(&[
            "uv",
            "sync",
            "--default-index=https://packages.elsewhere.example/simple"
        ]))
        .is_err());
        let mut extra = add.clone();
        extra.insert(2, "--extra-index-url=https://x.example/simple".into());
        assert!(guard(&extra).is_err());
        // After `--` everything is a package name, not an option.
        assert!(
            guard(&line(&["uv", "add", "--", INDEX_FLAG]))
                .unwrap()
                .env
                .len()
                == 2
        );
    }

    #[test]
    fn remote_install_flag_is_quoted_and_authorized() {
        wb_net::gate()
            .allow_package_index(&package_index())
            .unwrap();
        let flag = remote_index_flag("uv sync (remote)").unwrap();
        assert_eq!(
            flag,
            format!(
                "--default-index {}",
                crate::util::sh_quote(&package_index())
            )
        );
        assert_eq!(
            wb_net::DEFAULT_PACKAGE_INDEX,
            fibration_protocol::config::DEFAULT_PACKAGE_INDEX
        );
        let prefix = remote_bwrap_prefix();
        assert!(prefix.starts_with("bwrap --unshare-net ") && prefix.ends_with(" -- "));
        assert!(remote_bwrap_probe_command().starts_with(&format!("{prefix}true ")));
    }
}
