//! Run a command with no network, and nothing else changed.
//!
//! Used for the processes the app keeps alive itself: the Python kernels and the embedded
//! terminal. They keep the user's view of the file system (file confinement of agent code is
//! a separate, stricter sandbox applied per run) but cannot open a network connection.
//! Outbound access belongs to the network gate in the Rust core alone.
//!
//! * **Linux:** `bwrap --unshare-net` with the whole file system bound as it is. A stricter
//!   `bwrap` sandbox can still be started from inside (the kernels do that for each run).
//! * **macOS:** `sandbox-exec` with a profile that allows everything except `network*`.
//! * **Elsewhere, or where the tool does not work** (for example Linux hosts that forbid
//!   unprivileged user namespaces): no isolation. [`wrap`] says so, and callers must show it.

use std::ffi::OsString;
use std::sync::OnceLock;

/// How a wrapped command is kept off the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetIsolation {
    /// Linux `bwrap --unshare-net`.
    Bubblewrap,
    /// macOS `sandbox-exec`.
    SandboxExec,
    /// No tool available; the string says why. The command runs with network access.
    None(String),
}

impl NetIsolation {
    pub fn is_isolated(&self) -> bool {
        !matches!(self, NetIsolation::None(_))
    }

    /// A short name for status displays: `bwrap`, `sandbox-exec`, or `none`.
    pub fn label(&self) -> &'static str {
        match self {
            NetIsolation::Bubblewrap => "bwrap",
            NetIsolation::SandboxExec => "sandbox-exec",
            NetIsolation::None(_) => "none",
        }
    }
}

/// The `bwrap` arguments placed before `--` and the command. The root is bound with device
/// access (`--dev-bind`): a plain `--bind` makes `/dev/null` and the terminal's pty unusable. Also valid on a remote Linux
/// host, so callers that build a remote command line can reuse them.
pub const BWRAP_NO_NETWORK_ARGS: &[&str] =
    &["--unshare-net", "--die-with-parent", "--dev-bind", "/", "/"];

/// The `sandbox-exec` profile: everything allowed except the network.
pub const SANDBOX_EXEC_NO_NETWORK_PROFILE: &str = "(version 1)(allow default)(deny network*)";

/// A command line ready to spawn, and how it is isolated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    pub argv: Vec<OsString>,
    pub isolation: NetIsolation,
}

/// Which isolation works on this machine. Probed once by running the tool on a no-op.
pub fn probe() -> &'static NetIsolation {
    static PROBE: OnceLock<NetIsolation> = OnceLock::new();
    PROBE.get_or_init(probe_now)
}

fn runs_ok(program: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("{program} could not be started: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "{program} is installed but does not work here: {}",
            err.lines().next().unwrap_or("it exited with an error")
        ))
    }
}

fn probe_now() -> NetIsolation {
    if cfg!(target_os = "linux") {
        let mut args: Vec<&str> = BWRAP_NO_NETWORK_ARGS.to_vec();
        args.extend(["--", "true"]);
        match runs_ok("bwrap", &args) {
            Ok(()) => NetIsolation::Bubblewrap,
            Err(why) => NetIsolation::None(why),
        }
    } else if cfg!(target_os = "macos") {
        match runs_ok(
            "/usr/bin/sandbox-exec",
            &["-p", SANDBOX_EXEC_NO_NETWORK_PROFILE, "/usr/bin/true"],
        ) {
            Ok(()) => NetIsolation::SandboxExec,
            Err(why) => NetIsolation::None(why),
        }
    } else {
        NetIsolation::None("no network sandbox is implemented for this platform".to_string())
    }
}

/// Wrap `argv` (program first) so it runs with no network, using [`probe`]. When no isolation
/// is available the command is returned unchanged with [`NetIsolation::None`].
pub fn wrap<I, S>(argv: I) -> Wrapped
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    wrap_with(probe(), argv)
}

/// [`wrap`] with a given isolation, for callers that probed elsewhere and for tests.
pub fn wrap_with<I, S>(isolation: &NetIsolation, argv: I) -> Wrapped
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let command: Vec<OsString> = argv.into_iter().map(Into::into).collect();
    let mut out: Vec<OsString> = Vec::new();
    match isolation {
        NetIsolation::Bubblewrap => {
            out.push("bwrap".into());
            out.extend(BWRAP_NO_NETWORK_ARGS.iter().map(OsString::from));
            out.push("--".into());
        }
        NetIsolation::SandboxExec => {
            out.push("/usr/bin/sandbox-exec".into());
            out.push("-p".into());
            out.push(SANDBOX_EXEC_NO_NETWORK_PROFILE.into());
        }
        NetIsolation::None(_) => {}
    }
    out.extend(command);
    Wrapped {
        argv: out,
        isolation: isolation.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_per_isolation_kind() {
        let w = wrap_with(&NetIsolation::Bubblewrap, ["python3", "-m", "k"]);
        let argv: Vec<String> = w
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            [
                "bwrap",
                "--unshare-net",
                "--die-with-parent",
                "--dev-bind",
                "/",
                "/",
                "--",
                "python3",
                "-m",
                "k"
            ]
        );
        let w = wrap_with(&NetIsolation::SandboxExec, ["sh"]);
        assert_eq!(w.argv.len(), 4);
        assert_eq!(w.argv[0], OsString::from("/usr/bin/sandbox-exec"));
        let none = NetIsolation::None("why".into());
        let w = wrap_with(&none, ["sh", "-c", "x"]);
        assert_eq!(w.argv.len(), 3);
        assert!(!w.isolation.is_isolated());
        assert_eq!(none.label(), "none");
    }

    /// With a working tool, a wrapped process cannot connect out, can still write where the
    /// user can, and a stricter nested sandbox still starts.
    #[test]
    fn a_wrapped_process_has_no_network() {
        let isolation = probe();
        if !isolation.is_isolated() {
            eprintln!("skipped: {isolation:?}");
            return;
        }
        // A UDP "connect" needs a route but sends nothing; without a network it fails.
        let script = "import socket,sys\ns=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)\ntry:\n s.connect(('192.0.2.1',9))\n print('ROUTED')\nexcept OSError:\n print('NO-NETWORK')\n";
        let python = ["python3", "python"].into_iter().find(|p| {
            std::process::Command::new(p)
                .arg("--version")
                .output()
                .is_ok()
        });
        let Some(python) = python else {
            eprintln!("skipped: no python");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("written");
        let w = wrap([python, "-c", script]);
        let out = std::process::Command::new(&w.argv[0])
            .args(&w.argv[1..])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "NO-NETWORK",
            "{out:?}"
        );

        // Device nodes stay usable (kernels pass /dev/null to their children).
        let w = wrap([
            "sh",
            "-c",
            &format!("echo x > /dev/null && echo ok > '{}'", marker.display()),
        ]);
        let st = std::process::Command::new(&w.argv[0])
            .args(&w.argv[1..])
            .status()
            .unwrap();
        assert!(st.success());
        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "ok");

        if *isolation == NetIsolation::Bubblewrap {
            let w = wrap([
                "bwrap",
                "--unshare-all",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "true",
            ]);
            let st = std::process::Command::new(&w.argv[0])
                .args(&w.argv[1..])
                .status()
                .unwrap();
            assert!(st.success(), "a nested bwrap sandbox must still start");
        }
    }
}
