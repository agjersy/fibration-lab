//! `KernelTransport`: how a kernel process is launched. `LocalStdio` spawns the tooling
//! interpreter directly; `SshStdio` spawns `ssh <host> <same command>` and uses the SSH
//! process's stdio. Nothing above the transport knows which one is in use.
//!
//! A kernel never has network access. The local kernel is spawned through
//! `wb_sandbox::no_network::wrap` (the per-run sandboxes it starts are stricter still), and
//! the remote kernel is wrapped with `bwrap --unshare-net` on the host when that works
//! there. [`KernelTransport::network_isolation`] reports which, for display.

use analyst_protocol::config::GlobalConfig;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Arguments of `python -m analyst_kernel` (PROTOCOL 1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelArgs {
    /// Session interpreter (`--python`).
    pub python: String,
    /// Repo or mirror root (`--root`).
    pub root: String,
    /// `--runs-dir`.
    pub runs_dir: String,
    /// `--out-dir`, when not the default `out`.
    pub out_dir: Option<String>,
    /// `--sandbox auto|bwrap|sandbox-exec|none`.
    pub sandbox: Option<String>,
    /// External data directories approved by the user (the global config's target
    /// `data_dirs`), emitted as repeated `--data-dir <abs path>`.
    pub data_dirs: Vec<String>,
}

impl KernelArgs {
    pub fn to_args(&self) -> Vec<String> {
        let mut v = vec![
            "--python".to_string(),
            self.python.clone(),
            "--root".to_string(),
            self.root.clone(),
            "--runs-dir".to_string(),
            self.runs_dir.clone(),
        ];
        if let Some(o) = &self.out_dir {
            v.push("--out-dir".into());
            v.push(o.clone());
        }
        if let Some(s) = &self.sandbox {
            v.push("--sandbox".into());
            v.push(s.clone());
        }
        for d in &self.data_dirs {
            v.push("--data-dir".into());
            v.push(d.clone());
        }
        v
    }
}

/// The kernel's child process, in a process group of its own.
///
/// Killing only the direct child is not enough when the kernel runs under a wrapper
/// (`bwrap`, `sandbox-exec`): the wrapper dies and the kernel can stay behind. `bwrap
/// --die-with-parent` relies on the parent-death signal, which the kernel clears when the
/// wrapped program starts under a changed security profile (as with the AppArmor profile
/// recent Ubuntu releases apply to `bwrap`). So the whole group is killed, both when the
/// handle is dropped and before a reconnect. Detached runs start their own sessions and are
/// not in this group.
pub struct KernelChild {
    child: Child,
    #[cfg(unix)]
    group: Option<i32>,
}

impl KernelChild {
    /// Kill every process in the kernel's group. Does nothing once the child has been
    /// reaped, since its process-group id may then belong to something else.
    pub fn kill_group(&self) {
        #[cfg(unix)]
        if let (Some(group), Some(_)) = (self.group, self.child.id()) {
            // SAFETY: killpg has no memory-safety preconditions; the group id is this
            // child's own, and the child has not been reaped.
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
        }
    }
}

impl std::ops::Deref for KernelChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl std::ops::DerefMut for KernelChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for KernelChild {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// A spawned kernel process with its stdio handles.
pub struct SpawnedKernel {
    pub child: KernelChild,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

#[async_trait]
pub trait KernelTransport: Send + Sync {
    /// The full local command line (program first), for logging and tests.
    fn command_line(&self) -> Vec<String>;

    /// Spawn the kernel process.
    async fn spawn(&self) -> Result<SpawnedKernel> {
        spawn_line(&self.command_line())
    }

    fn describe(&self) -> String {
        self.command_line().join(" ")
    }

    /// How the kernel process is kept off the network: `bwrap`, `sandbox-exec`, or `none`
    /// (no isolation available; the kernel then has the user's network access).
    fn network_isolation(&self) -> String {
        "none".to_string()
    }
}

/// Environment variables that commonly hold provider API keys; never passed to a kernel.
pub const PROVIDER_KEY_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "DEEPSEEK_API_KEY",
];

/// Spawn a kernel command line with piped stdio. The child is killed when its handle drops.
pub fn spawn_line(line: &[String]) -> Result<SpawnedKernel> {
    spawn_line_without_env(line, &[])
}

/// [`spawn_line`], additionally removing `env_remove` (the configured providers'
/// `api_key_env` names) from the child's environment. The common provider key variables are
/// always removed: the kernel and the scripts it runs have no use for them.
pub fn spawn_line_without_env(line: &[String], env_remove: &[String]) -> Result<SpawnedKernel> {
    let (program, args) = line
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty kernel command line"))?;
    // An `ssh` line only reaches a configured SSH target (and is recorded in the ledger).
    let guard = crate::egress::guard(line)?;
    // Bare program names are resolved through the widened tool search path, and the
    // kernel inherits that PATH so it can find `uv`, `rg`, and the sandbox helpers.
    let mut cmd = Command::new(crate::util::resolve_tool(program));
    cmd.args(args)
        .env("PATH", crate::util::tool_path_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    for name in PROVIDER_KEY_VARS {
        cmd.env_remove(name);
    }
    for name in env_remove {
        cmd.env_remove(name);
    }
    guard.apply_tokio(&mut cmd);
    // Any `uv` the kernel starts (the package listing, a script's own call) uses an
    // interpreter that is installed and never downloads one.
    cmd.env(
        crate::egress::NO_PYTHON_DOWNLOADS.0,
        crate::egress::NO_PYTHON_DOWNLOADS.1,
    );
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning kernel: {}", line.join(" ")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("kernel stdin not captured"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("kernel stdout not captured"))?;
    let child = KernelChild {
        #[cfg(unix)]
        group: child.id().and_then(|pid| i32::try_from(pid).ok()),
        child,
    };
    Ok(SpawnedKernel {
        child,
        stdin,
        stdout,
    })
}

/// Local kernel: `<launcher...> --python .. --root .. --runs-dir .. [--out-dir ..] [--sandbox ..]`.
/// The launcher is normally `[<tooling_python>, "-m", "analyst_kernel"]`; tests inject
/// `["python3", "<fake_kernel.py>"]`.
#[derive(Debug, Clone)]
pub struct LocalStdio {
    pub launcher: Vec<String>,
    pub args: KernelArgs,
    /// Extra environment variables removed from the kernel process.
    pub env_remove: Vec<String>,
}

impl LocalStdio {
    pub fn new(tooling_python: impl Into<String>, args: KernelArgs) -> Self {
        LocalStdio {
            launcher: vec![tooling_python.into(), "-m".into(), "analyst_kernel".into()],
            args,
            env_remove: vec![],
        }
    }

    pub fn with_launcher(launcher: Vec<String>, args: KernelArgs) -> Self {
        LocalStdio {
            launcher,
            args,
            env_remove: vec![],
        }
    }

    /// Remove these variables from the kernel's environment.
    pub fn without_env(mut self, names: Vec<String>) -> Self {
        self.env_remove = names;
        self
    }
}

impl LocalStdio {
    /// The kernel command itself, before the no-network wrapper.
    pub fn kernel_command(&self) -> Vec<String> {
        let mut v = self.launcher.clone();
        v.extend(self.args.to_args());
        v
    }
}

#[async_trait]
impl KernelTransport for LocalStdio {
    /// The kernel command wrapped so the process has no network (`bwrap --unshare-net` on
    /// Linux, `sandbox-exec` on macOS; unchanged where neither works).
    fn command_line(&self) -> Vec<String> {
        // The program is resolved before wrapping: inside the wrapper nothing widens PATH.
        let mut command = self.kernel_command();
        if let Some(program) = command.first_mut() {
            *program = crate::util::resolve_tool(program)
                .to_string_lossy()
                .to_string();
        }
        wb_sandbox::no_network::wrap(command)
            .argv
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    fn network_isolation(&self) -> String {
        wb_sandbox::no_network::probe().label().to_string()
    }

    async fn spawn(&self) -> Result<SpawnedKernel> {
        spawn_line_without_env(&self.command_line(), &self.env_remove)
    }
}

/// Remote kernel over the system OpenSSH client with connection multiplexing.
#[derive(Debug, Clone)]
pub struct SshStdio {
    pub host: String,
    /// `$ANALYST_HOME/ssh` for control sockets; `None` disables multiplexing.
    pub control_dir: Option<PathBuf>,
    /// Remote launcher, e.g. `["~/.analyst/tooling/.venv/bin/python", "-m", "analyst_kernel"]`.
    pub remote_launcher: Vec<String>,
    pub args: KernelArgs,
    /// Wrap the remote kernel in `bwrap --unshare-net` (set when the host's probe says
    /// bubblewrap works there, see `egress::remote_bwrap_works`).
    pub no_network: bool,
}

/// Options for every ssh and scp invocation, and for the ssh that git runs for a push: connection multiplexing (when a
/// control directory is given), batch mode, a connect timeout, and keepalives so a dead
/// connection (laptop sleep, network change) fails within about 45 s instead of hanging.
pub fn ssh_options(control_dir: Option<&Path>) -> Vec<String> {
    let mut v = Vec::new();
    let mut opt = |o: String| {
        v.push("-o".to_string());
        v.push(o);
    };
    if let Some(dir) = control_dir {
        opt("ControlMaster=auto".into());
        opt(format!("ControlPath={}/%C", dir.display()));
        opt("ControlPersist=10m".into());
    }
    opt("BatchMode=yes".into());
    opt("ConnectTimeout=10".into());
    opt("ServerAliveInterval=15".into());
    opt("ServerAliveCountMax=3".into());
    v
}

/// `ssh <options> -- <host>` prefix. The `--` keeps a host from being read as an option;
/// hosts starting with `-` are additionally rejected by `target::validate_host`.
pub fn ssh_prefix(host: &str, control_dir: Option<&Path>) -> Vec<String> {
    let mut v = vec!["ssh".to_string()];
    v.extend(ssh_options(control_dir));
    v.push("--".into());
    v.push(host.to_string());
    v
}

impl SshStdio {
    pub fn remote_command(&self) -> String {
        let mut parts: Vec<String> = self.remote_launcher.clone();
        parts.extend(self.args.to_args());
        let command = parts
            .iter()
            // `~/...` must stay unquoted so the remote shell expands it.
            .map(|p| crate::util::sh_quote_path(p))
            .collect::<Vec<_>>()
            .join(" ");
        if self.no_network {
            format!("{}{command}", crate::egress::remote_bwrap_prefix())
        } else {
            command
        }
    }
}

#[async_trait]
impl KernelTransport for SshStdio {
    fn command_line(&self) -> Vec<String> {
        let mut v = ssh_prefix(&self.host, self.control_dir.as_deref());
        // The remote kernel, like every remote command, may not download Python.
        v.push(format!(
            "{}{}",
            crate::egress::remote_env_prefix(),
            self.remote_command()
        ));
        v
    }

    async fn spawn(&self) -> Result<SpawnedKernel> {
        crate::target::validate_host(&self.host)?;
        spawn_line(&self.command_line())
    }

    fn network_isolation(&self) -> String {
        crate::egress::remote_isolation_label(self.no_network).to_string()
    }
}

/// Resolve the interpreter of the app tooling environment (the one that hosts
/// `analyst_kernel`), in this order:
///
/// 1. `ANALYST_TOOLING_PYTHON` environment variable.
/// 2. `tooling_python` in the global config.
/// 3. Dev mode: `<workspace>/python/kernel/.venv/bin/python` if it exists, where `<workspace>`
///    is found by walking up from the current executable's directory and then from the current
///    working directory, looking for a directory containing `python/kernel`.
/// 4. The bootstrapped tooling environment `$ANALYST_HOME/tooling/.venv` (see
///    `target::bootstrap::ensure_local`).
/// 5. `python3` found on the tool search path.
pub fn resolve_tooling_python(global: &GlobalConfig) -> String {
    resolve_tooling_python_in(global, &crate::config::analyst_home())
}

/// [`resolve_tooling_python`] for an explicit analyst home.
pub fn resolve_tooling_python_in(global: &GlobalConfig, home: &Path) -> String {
    if let Ok(v) = std::env::var("ANALYST_TOOLING_PYTHON") {
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(v) = &global.tooling_python {
        if !v.is_empty() {
            return crate::util::expand_tilde(v).to_string_lossy().to_string();
        }
    }
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            starts.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    for start in starts {
        for dir in start.ancestors() {
            let candidate = dir.join("python/kernel/.venv/bin/python");
            if candidate.is_file() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    let bootstrapped = crate::target::bootstrap::local_tooling_python(home);
    if bootstrapped.is_file() {
        return bootstrapped.to_string_lossy().to_string();
    }
    crate::util::tool("python3")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> KernelArgs {
        KernelArgs {
            python: "/repo/.venv/bin/python".into(),
            root: "/repo".into(),
            runs_dir: "/home/u/.analyst/runs".into(),
            out_dir: None,
            sandbox: Some("auto".into()),
            data_dirs: vec![],
        }
    }

    #[test]
    fn data_dirs_become_repeated_arguments() {
        let mut a = args();
        a.data_dirs = vec!["/data/shared".into(), "/mnt/my data".into()];
        let v = a.to_args();
        assert_eq!(
            v[v.len() - 4..],
            ["--data-dir", "/data/shared", "--data-dir", "/mnt/my data"]
        );
        let t = SshStdio {
            host: "h".into(),
            control_dir: None,
            remote_launcher: vec!["python".into()],
            args: a,
            no_network: false,
        };
        assert!(t
            .remote_command()
            .ends_with("--data-dir /data/shared --data-dir '/mnt/my data'"));
    }

    #[tokio::test]
    async fn ssh_transport_rejects_option_like_hosts() {
        let t = SshStdio {
            host: "-oProxyCommand=touch /tmp/pwned".into(),
            control_dir: None,
            remote_launcher: vec!["python".into()],
            args: args(),
            no_network: false,
        };
        assert!(t.spawn().await.is_err());
    }

    #[test]
    fn local_command_line() {
        let t = LocalStdio::new("/tooling/bin/python", args());
        // The spawned line is the kernel command behind the no-network wrapper.
        let line = t.command_line();
        let kernel = t.kernel_command();
        assert_eq!(line[line.len() - kernel.len()..], kernel[..]);
        match wb_sandbox::no_network::probe() {
            wb_sandbox::no_network::NetIsolation::Bubblewrap => {
                assert_eq!(line[..2], ["bwrap", "--unshare-net"]);
                assert_eq!(line[line.len() - kernel.len() - 1], "--");
                assert_eq!(t.network_isolation(), "bwrap");
            }
            wb_sandbox::no_network::NetIsolation::SandboxExec => {
                assert_eq!(line[0], "/usr/bin/sandbox-exec");
                assert_eq!(t.network_isolation(), "sandbox-exec");
            }
            wb_sandbox::no_network::NetIsolation::None(_) => {
                assert_eq!(line, kernel);
                assert_eq!(t.network_isolation(), "none");
            }
        }
        assert_eq!(
            kernel,
            vec![
                "/tooling/bin/python",
                "-m",
                "analyst_kernel",
                "--python",
                "/repo/.venv/bin/python",
                "--root",
                "/repo",
                "--runs-dir",
                "/home/u/.analyst/runs",
                "--sandbox",
                "auto"
            ]
        );
    }

    /// A kernel process started through the local transport cannot open a connection out,
    /// and neither can anything it starts.
    #[tokio::test]
    async fn a_local_kernel_process_has_no_network() {
        use tokio::io::AsyncReadExt;
        if !wb_sandbox::no_network::probe().is_isolated() {
            eprintln!("skipped: {:?}", wb_sandbox::no_network::probe());
            return;
        }
        // A UDP "connect" needs a route but sends nothing; without a network it fails. The
        // child process stands for a run started by the kernel.
        let probe = "import socket\ns=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)\ntry:\n s.connect(('192.0.2.1',9)); print('ROUTED')\nexcept OSError:\n print('NO-NETWORK')\n";
        let code = format!(
            "import subprocess,sys\nexec({probe:?})\nsys.stdout.flush()\nsubprocess.run([sys.executable,'-c',{probe:?}])\n"
        );
        let t = LocalStdio::with_launcher(vec!["python3".into(), "-c".into(), code], args());
        assert_ne!(t.network_isolation(), "none");
        let mut spawned = t.spawn().await.unwrap();
        let mut out = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            spawned.stdout.read_to_string(&mut out),
        )
        .await
        .unwrap()
        .unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines, ["NO-NETWORK", "NO-NETWORK"], "{out:?}");
    }

    /// An ssh kernel line is refused unless its host is a configured SSH target.
    #[tokio::test]
    async fn ssh_transport_needs_a_configured_target() {
        let host = format!("unconfigured-{}", crate::util::short_rand().to_lowercase());
        let t = SshStdio {
            host: host.clone(),
            control_dir: None,
            remote_launcher: vec!["python".into()],
            args: args(),
            no_network: false,
        };
        let err = match t.spawn().await {
            Ok(_) => panic!("an unconfigured host was contacted"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains(&host) && err.contains("config.toml"), "{err}");
    }

    #[test]
    fn ssh_command_line() {
        let t = SshStdio {
            host: "lab-gpu".into(),
            control_dir: Some(PathBuf::from("/home/u/.analyst/ssh")),
            remote_launcher: vec![
                "~/.analyst/tooling/.venv/bin/python".into(),
                "-m".into(),
                "analyst_kernel".into(),
            ],
            args: KernelArgs {
                python: "~/analyst/repo/.venv/bin/python".into(),
                root: "~/analyst/repo".into(),
                runs_dir: "~/.analyst/runs".into(),
                out_dir: None,
                sandbox: Some("auto".into()),
                data_dirs: vec![],
            },
            no_network: false,
        };
        assert_eq!(t.network_isolation(), "none");
        let wrapped = SshStdio {
            no_network: true,
            ..t.clone()
        };
        assert_eq!(wrapped.network_isolation(), "bwrap");
        assert_eq!(
            wrapped.remote_command(),
            format!(
                "{}{}",
                crate::egress::remote_bwrap_prefix(),
                t.remote_command()
            )
        );
        let line = t.command_line();
        assert_eq!(line[0], "ssh");
        assert_eq!(line[1..3], ["-o", "ControlMaster=auto"]);
        assert_eq!(line[3..5], ["-o", "ControlPath=/home/u/.analyst/ssh/%C"]);
        assert_eq!(line[5..7], ["-o", "ControlPersist=10m"]);
        assert_eq!(
            line[7..15],
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3"
            ]
        );
        assert_eq!(line[15..17], ["--", "lab-gpu"]);
        assert_eq!(
            line[17],
            "export UV_PYTHON_DOWNLOADS=never; ~/.analyst/tooling/.venv/bin/python -m analyst_kernel --python ~/analyst/repo/.venv/bin/python --root ~/analyst/repo --runs-dir ~/.analyst/runs --sandbox auto"
        );
    }

    #[test]
    fn tooling_python_env_wins() {
        let g = GlobalConfig {
            tooling_python: Some("/cfg/python".into()),
            ..Default::default()
        };
        // Cannot set env in parallel tests safely; only check the config path and default order.
        if std::env::var("ANALYST_TOOLING_PYTHON").is_err() {
            assert_eq!(resolve_tooling_python(&g), "/cfg/python");
        }
    }
}
