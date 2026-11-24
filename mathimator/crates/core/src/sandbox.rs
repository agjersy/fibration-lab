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

/// As [`no_network_command`], for a process that must still reach one Unix socket (the cell
/// runner and the router's attach socket).
///
/// On Linux nothing more is needed: a path-addressed socket works inside the network
/// namespace. On macOS `(deny network*)` also covers Unix sockets, so the profile allows
/// outbound connections to exactly this socket path, in its given and canonical forms
/// (`/var/folders` is `/private/var/folders`).
pub fn no_network_command_with_socket<I, S>(
    program: &OsStr,
    args: I,
    socket: &std::path::Path,
) -> (Command, String)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<std::ffi::OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    if std::env::var_os("MATHIMATOR_NO_SANDBOX").is_none()
        && matches!(no_network::probe(), NetIsolation::SandboxExec)
    {
        let mut argv: Vec<std::ffi::OsString> = vec![
            "/usr/bin/sandbox-exec".into(),
            "-p".into(),
            sandbox_exec_profile(socket).into(),
            program.to_owned(),
        ];
        argv.extend(args);
        return (command(&argv), "sandbox-exec".to_string());
    }
    no_network_command(program, args)
}

/// The `sandbox-exec` profile for a process allowed one Unix socket and no other network.
/// Later rules win, so the allow follows the deny.
pub fn sandbox_exec_profile(socket: &std::path::Path) -> String {
    let mut paths = vec![socket.to_path_buf()];
    // The socket file may not exist yet when the profile is built; canonicalize its directory.
    if let (Some(dir), Some(name)) = (socket.parent(), socket.file_name()) {
        if let Ok(real) = dir.canonicalize() {
            let real = real.join(name);
            if real != socket {
                paths.push(real);
            }
        }
    }
    let mut profile = String::from("(version 1)(allow default)(deny network*)");
    for path in paths {
        let quoted = path
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        profile.push_str(&format!(
            "(allow network-outbound (remote unix-socket (path-literal \"{quoted}\")))"
        ));
    }
    profile
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_macos_profile_allows_only_the_attach_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("a\"b.sock");
        let profile = sandbox_exec_profile(&socket);
        assert!(
            profile.starts_with("(version 1)(allow default)(deny network*)(allow network-outbound")
        );
        // The deny comes first, so the later allow wins for this one path only.
        assert!(profile.contains("a\\\"b.sock\")))"));
        assert_eq!(profile.matches("deny network*").count(), 1);
        assert!(profile.matches("allow network-outbound").count() >= 1);
    }
}
