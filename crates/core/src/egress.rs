//! The core's side of the network gate for subprocesses (ARCHITECTURE 5.6).
//!
//! `wb_net` builds every HTTP request itself, but package installs and SSH run as child
//! processes (`uv`, `ssh`, `scp`, `rsync`) that it cannot intercept. Everything in this crate
//! that spawns a command line passes it through [`guard`] first, which is the one place those
//! programs are recognised:
//!
//! - `ssh`, `scp`, and `rsync` to a remote host are authorized against the gate
//!   ([`wb_net::Gate::authorize_ssh`]): the host must be a configured SSH target, and the
//!   decision is written to the egress ledger. A line whose host cannot be determined is
//!   refused.
//! - `uv` is an install only when the line names the configured package index with
//!   [`INDEX_FLAG`] (lines built by [`uv_install`]); the index is then authorized
//!   ([`wb_net::Gate::authorize_package_index`]). Every other `uv` invocation gets
//!   [`wb_net::OFFLINE_ENV`], so it fails rather than contact an index.
//!
//! `uv` commands that run on a remote host inside an `ssh` line are built with
//! [`remote_index_flag`], which authorizes the index before it hands out the flag.

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
            "the network ledger (egress.jsonl in the analyst home) could not be written ({why}); nothing leaves the machine unrecorded, so the command was not run"
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

/// What [`guard`] decided for a command line: changes to make to the child's environment.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Guard {
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

impl Guard {
    pub fn apply_tokio(&self, cmd: &mut tokio::process::Command) {
        for name in &self.env_remove {
            cmd.env_remove(name);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
    }

    pub fn apply_std(&self, cmd: &mut std::process::Command) {
        for name in &self.env_remove {
            cmd.env_remove(name);
        }
        for (k, v) in &self.env {
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

/// The host of `host:path` (scp and rsync remote operands); `None` for a local path.
fn remote_operand_host(arg: &str) -> Option<&str> {
    if let Some(rest) = arg.strip_prefix("rsync://") {
        return rest.split(['/', ':']).next().filter(|h| !h.is_empty());
    }
    let (host, _) = arg.split_once(':')?;
    // A colon after a slash, or a leading `.`/`/`, is part of a local path.
    if host.is_empty() || host.contains('/') || host.starts_with('.') {
        return None;
    }
    Some(host)
}

/// The remote hosts an `ssh`, `scp`, or `rsync` line would connect to. An error means the
/// line cannot be understood, and is therefore not run.
fn ssh_hosts(name: &str, args: &[String]) -> Result<Vec<String>> {
    match name {
        "ssh" => {
            // Lines are built by `ssh_prefix`: options, `--`, then the host.
            let at = args.iter().position(|a| a == "--").ok_or_else(|| {
                anyhow!("refusing an ssh command line without `--` before the host")
            })?;
            let host = args
                .get(at + 1)
                .ok_or_else(|| anyhow!("the ssh command line names no host"))?;
            Ok(vec![host.clone()])
        }
        "scp" => {
            let at = args.iter().position(|a| a == "--").ok_or_else(|| {
                anyhow!("refusing an scp command line without `--` before the paths")
            })?;
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
        _ => {
            // rsync: operands are everything that is not an option or an option's value. With
            // no remote operand it is a local copy.
            let mut hosts = Vec::new();
            let mut skip = false;
            for a in args {
                if skip {
                    skip = false;
                    continue;
                }
                if a == "-e" || a == "--rsh" {
                    skip = true;
                    continue;
                }
                if a.starts_with('-') {
                    continue;
                }
                if let Some(h) = remote_operand_host(a) {
                    hosts.push(h.to_string());
                }
            }
            Ok(hosts)
        }
    }
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

/// Check a command line before it is spawned and say how its environment must change. See
/// the module documentation. Lines that run neither `uv` nor an SSH client pass unchanged.
pub fn guard(line: &[String]) -> Result<Guard> {
    let Some((program, args)) = line.split_first() else {
        return Ok(Guard::default());
    };
    let name = program_name(program);
    match name.as_str() {
        "ssh" | "scp" | "rsync" | "sftp" => {
            if name == "sftp" {
                return Err(anyhow!("sftp is not used by the app and is not allowed"));
            }
            for host in ssh_hosts(&name, args)? {
                wb_net::gate()
                    .authorize_ssh(&host, &name)
                    .map_err(|e| refusal(&e))?;
            }
            Ok(Guard::default())
        }
        "uv" | "uvx" | "pip" | "pip3" => {
            let env_remove: Vec<String> = INDEX_ENV_VARS.iter().map(|s| s.to_string()).collect();
            match index_of(args)? {
                Some(index) => {
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
                    })
                }
                None => Ok(Guard {
                    env: wb_net::OFFLINE_ENV
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    env_remove,
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
        "{}true >/dev/null 2>&1 && echo analyst-bwrap-yes || echo analyst-bwrap-no",
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
    let works = out.contains("analyst-bwrap-yes");
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
            &format!("me@{host}:.analyst/bin/uv"),
        ]);
        assert!(guard(&scp).is_err());
        let rsync = crate::target::sync::rsync_command(&host, "~/m", Path::new("/tmp/l"), None);
        assert!(guard(&rsync).is_err());

        wb_net::gate().allow_ssh_host(&host).unwrap();
        assert_eq!(guard(&ssh).unwrap(), Guard::default());
        assert!(guard(&scp).is_ok());
        assert!(guard(&rsync).is_ok());
        // A resolved path to the client is recognised too.
        let mut abs = ssh.clone();
        abs[0] = "/usr/bin/ssh".into();
        assert!(guard(&abs).is_ok());
        let host_at = abs.len() - 2;
        abs[host_at] = format!("other-{tag}");
        assert!(guard(&abs).is_err());

        // Lines that cannot be understood are refused; a local rsync is not a connection.
        assert!(guard(&line(&["ssh", &host, "true"])).is_err());
        assert!(guard(&line(&["scp", "--", "/a", "/b"])).is_err());
        assert!(guard(&line(&["sftp", &host])).is_err());
        assert!(guard(&line(&[
            "rsync",
            "-az",
            "--files-from=/tmp/l",
            "./",
            "/dest/"
        ]))
        .is_ok());
        assert!(guard(&line(&["rsync", "-az", "./", &format!("other-{tag}:x/")])).is_err());
        assert!(guard(&line(&[
            "rsync",
            "-az",
            &format!("rsync://other-{tag}/m"),
            "./"
        ]))
        .is_err());
        assert_eq!(guard(&line(&["git", "status"])).unwrap(), Guard::default());
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
            analyst_protocol::config::DEFAULT_PACKAGE_INDEX
        );
        let prefix = remote_bwrap_prefix();
        assert!(prefix.starts_with("bwrap --unshare-net ") && prefix.ends_with(" -- "));
        assert!(remote_bwrap_probe_command().starts_with(&format!("{prefix}true ")));
    }
}
