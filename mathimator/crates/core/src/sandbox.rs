//! Isolation for the processes math-core starts (ARCHITECTURE 5.1, 6.3, 16).
//!
//! The engine and the cell runner get no network, through the same wrapper Analyst uses for
//! its kernels and terminal (`wb_sandbox::no_network`: `bwrap --unshare-net` on Linux,
//! `sandbox-exec` with the network denied on macOS). Path-addressed Unix sockets keep working
//! inside the network namespace, so the runner still reaches the router.
//!
//! Not yet here: the filesystem confinement of an Analyst run (read-only project, writable
//! output directory only). It comes with the Analyst pairing (BUILD-SPEC phase 2), where cells
//! run through the kernel's run sandbox; until then the runner can read and write what the user
//! can, apart from the network.

use std::ffi::OsStr;

use tokio::process::Command;
use wb_sandbox::no_network::{self, NetIsolation};

/// `program args...` wrapped so it has no network, and a label for the isolation used
/// (`bwrap`, `sandbox-exec`, or `none: <why>`).
pub fn no_network_command<I, S>(program: &OsStr, args: I) -> (Command, String)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut argv: Vec<std::ffi::OsString> = vec![program.to_owned()];
    argv.extend(args.into_iter().map(|a| a.as_ref().to_owned()));
    if std::env::var_os("MATHIMATOR_NO_SANDBOX").is_some() {
        return (
            command(&argv),
            "none: MATHIMATOR_NO_SANDBOX is set".to_string(),
        );
    }
    let wrapped = no_network::wrap(argv);
    (command(&wrapped.argv), label(&wrapped.isolation))
}

fn command(argv: &[std::ffi::OsString]) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd
}

fn label(isolation: &NetIsolation) -> String {
    match isolation {
        NetIsolation::Bubblewrap => "bwrap".to_string(),
        NetIsolation::SandboxExec => "sandbox-exec".to_string(),
        NetIsolation::None(why) => format!("none: {why}"),
    }
}
