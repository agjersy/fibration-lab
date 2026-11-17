//! Shared crate `wb-sandbox`. Must not depend on workbench-specific crates.
//!
//! [`no_network`] wraps a long-lived process (a kernel, the terminal) so it has no network.
//!
//! Launches one command inside the code sandbox described in PROTOCOL.md section 9.
//!
//! * **Windows** (the real implementation, `windows.rs`): a per-run AppContainer profile with no
//!   capabilities (so no network), temporary ACEs for the AppContainer SID on the listed
//!   directories (read + execute on `read`, modify on `write`), and a job object carrying the
//!   memory, process-count, and UI limits and `KILL_ON_JOB_CLOSE`.
//! * **Elsewhere** (`unix.rs`): no isolation. The command runs directly with `RLIMIT_AS` and
//!   `RLIMIT_NPROC` applied where the platform honours them. macOS (`sandbox-exec`) and Linux
//!   (`bwrap`) isolation is implemented by the Python kernel itself
//!   (`python/kernel/present_kernel/runner/sandbox.py`), which only uses this crate's
//!   `present-sandbox` binary on Windows.
//!
//! **The Windows path temporarily mutates the DACLs of the listed directories.** See
//! `docs/notes/sandbox-spike.md` for what that means and how stale entries are cleaned up.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod cli;
pub mod journal;
pub mod no_network;
pub mod quote;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Default memory cap (PROTOCOL 9): 2 GiB.
pub const DEFAULT_MEMORY: u64 = 2 * 1024 * 1024 * 1024;
/// Default active-process cap (PROTOCOL 9).
pub const DEFAULT_PROCESSES: u32 = 64;
/// Exit code of the `present-sandbox` binary when the helper itself fails (bad usage, the
/// sandbox could not be set up, the command could not be started). Mirrors `env(1)`.
pub const EXIT_HELPER_FAILURE: i32 = 125;
/// AppContainer profile names are `<prefix>.<pid>.<counter>.<time>`.
pub const PROFILE_PREFIX: &str = "present.sandbox";

/// What to run and what it may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    /// Working directory of the child. On Windows it must lie inside a `read` or `write` entry,
    /// or the child cannot open it.
    pub cwd: PathBuf,
    /// Directories (or files) the child may read and execute.
    pub read: Vec<PathBuf>,
    /// Directories (or files) the child may also modify.
    pub write: Vec<PathBuf>,
    /// Directories the child may enter and list, without access to what is in them: the
    /// working directory when only some of its subdirectories are granted.
    pub enter: Vec<PathBuf>,
    /// Memory cap in bytes, per process and for the whole job; `0` means no cap.
    pub memory: u64,
    /// Maximum number of live processes in the sandbox; `0` means no cap.
    pub processes: u32,
    /// Program followed by its arguments.
    pub command: Vec<OsString>,
    /// Environment of the child; `None` passes this process's environment through unchanged.
    pub env: Option<Vec<(OsString, OsString)>>,
    /// Kill the sandbox after this long; `None` waits forever.
    pub timeout: Option<Duration>,
}

impl SandboxSpec {
    /// A spec with the default limits, no directories, and the inherited environment.
    pub fn new<I, S>(cwd: impl Into<PathBuf>, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        SandboxSpec {
            cwd: cwd.into(),
            read: Vec::new(),
            write: Vec::new(),
            enter: Vec::new(),
            memory: DEFAULT_MEMORY,
            processes: DEFAULT_PROCESSES,
            command: command.into_iter().map(Into::into).collect(),
            env: None,
            timeout: None,
        }
    }

    /// Check the spec before anything is changed on disk.
    pub fn validate(&self) -> Result<(), SpecError> {
        let program = self.command.first().ok_or(SpecError::EmptyCommand)?;
        if program.is_empty() {
            return Err(SpecError::EmptyCommand);
        }
        for arg in &self.command {
            if arg.as_encoded_bytes().contains(&0) {
                return Err(SpecError::NulInArgument);
            }
        }
        if is_batch_file(Path::new(program)) {
            return Err(SpecError::BatchFile(PathBuf::from(program)));
        }
        if !self.cwd.is_dir() {
            return Err(SpecError::CwdNotADirectory(self.cwd.clone()));
        }
        for path in self.read.iter().chain(self.write.iter()) {
            if !path.is_absolute() {
                return Err(SpecError::RelativePath(path.clone()));
            }
            if path.parent().is_none() {
                return Err(SpecError::RootPath(path.clone()));
            }
            if !path.exists() {
                return Err(SpecError::MissingPath(path.clone()));
            }
        }
        if let Some(env) = &self.env {
            for (key, value) in env {
                let k = key.as_encoded_bytes();
                // A leading '=' is legal on Windows (the hidden per-drive variables).
                if k.is_empty()
                    || k.contains(&0)
                    || k[1..].contains(&b'=')
                    || value.as_encoded_bytes().contains(&0)
                {
                    return Err(SpecError::BadEnvironment(key.clone()));
                }
            }
        }
        Ok(())
    }

    /// `read` minus anything also listed in `write`, and `write`, each without duplicates
    /// (write access includes read access, so one ACE per path is enough).
    pub fn effective_dirs(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut write: Vec<PathBuf> = Vec::new();
        for p in &self.write {
            if !write.contains(p) {
                write.push(p.clone());
            }
        }
        let mut read: Vec<PathBuf> = Vec::new();
        for p in &self.read {
            if !write.contains(p) && !read.contains(p) {
                read.push(p.clone());
            }
        }
        (read, write)
    }
}

/// `CreateProcessW` runs `.bat`/`.cmd` files through `cmd.exe`, whose argument parsing does not
/// follow the `CommandLineToArgvW` rules; quoting for it safely is not possible in general, so
/// batch files are refused as the program.
fn is_batch_file(program: &Path) -> bool {
    let name = program.to_string_lossy();
    let name = name.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    name.ends_with(".bat") || name.ends_with(".cmd")
}

/// A spec that cannot be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    EmptyCommand,
    NulInArgument,
    BatchFile(PathBuf),
    CwdNotADirectory(PathBuf),
    RelativePath(PathBuf),
    /// Granting inheritable ACEs on a whole volume is never intended.
    RootPath(PathBuf),
    MissingPath(PathBuf),
    BadEnvironment(OsString),
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::EmptyCommand => write!(f, "no command given"),
            SpecError::NulInArgument => write!(f, "an argument contains a NUL character"),
            SpecError::BatchFile(p) => write!(
                f,
                "batch files cannot be run in the sandbox: {}",
                p.display()
            ),
            SpecError::CwdNotADirectory(p) => write!(
                f,
                "the working directory is not a directory: {}",
                p.display()
            ),
            SpecError::RelativePath(p) => {
                write!(f, "sandbox paths must be absolute: {}", p.display())
            }
            SpecError::RootPath(p) => write!(
                f,
                "a filesystem root cannot be granted to the sandbox: {}",
                p.display()
            ),
            SpecError::MissingPath(p) => write!(f, "sandbox path does not exist: {}", p.display()),
            SpecError::BadEnvironment(k) => {
                write!(f, "invalid environment variable: {}", k.to_string_lossy())
            }
        }
    }
}

impl std::error::Error for SpecError {}

impl From<SpecError> for io::Error {
    fn from(e: SpecError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, e)
    }
}

/// Result of a captured run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The child's exit code. On Windows this is the raw process exit code reinterpreted as
    /// `i32` (an NTSTATUS such as `0xC0000005` is negative); on Unix a signal death is
    /// `128 + signal`.
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// The timeout elapsed and the sandbox was killed.
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// Where the child's standard streams go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stdio {
    /// Inherit this process's stdin, stdout, and stderr.
    Inherit,
    /// stdin from the null device; stdout and stderr captured.
    Capture,
}

/// Whether this build isolates the child (filesystem and network), as opposed to applying
/// resource limits only. `true` only on Windows.
pub fn is_native_isolation() -> bool {
    cfg!(windows)
}

/// Run `spec.command` in the sandbox with stdin closed and stdout/stderr captured.
pub fn run_sandboxed(spec: &SandboxSpec) -> io::Result<Output> {
    spec.validate()?;
    platform_run(spec, Stdio::Capture)
}

/// Run `spec.command` in the sandbox with this process's standard streams inherited, and return
/// its exit code. This is what the `present-sandbox` binary does. If `spec.timeout` elapses the
/// sandbox is killed and the exit code is whatever the kill produced.
pub fn run_inherited(spec: &SandboxSpec) -> io::Result<i32> {
    spec.validate()?;
    platform_run(spec, Stdio::Inherit).map(|o| o.exit_code)
}

/// Process-wide preparation for a helper binary whose only job is to run one sandboxed child:
/// on Windows, console control events (Ctrl+C, Ctrl+Break, console close) no longer kill this
/// process outright; the child still receives them, exits, and the helper then cleans up the
/// ACEs and the profile. A no-op elsewhere.
pub fn prepare_helper_process() {
    #[cfg(windows)]
    windows::install_ctrl_handler();
}

#[cfg(windows)]
fn platform_run(spec: &SandboxSpec, stdio: Stdio) -> io::Result<Output> {
    windows::run(spec, stdio)
}

#[cfg(unix)]
fn platform_run(spec: &SandboxSpec, stdio: Stdio) -> io::Result<Output> {
    unix::run(spec, stdio)
}

#[cfg(not(any(unix, windows)))]
fn platform_run(_spec: &SandboxSpec, _stdio: Stdio) -> io::Result<Output> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "wb-sandbox does not support this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program() -> &'static str {
        if cfg!(windows) {
            "cmd.exe"
        } else {
            "/bin/sh"
        }
    }

    #[test]
    fn defaults() {
        let spec = SandboxSpec::new(".", ["x"]);
        assert_eq!(spec.memory, 2 * 1024 * 1024 * 1024);
        assert_eq!(spec.processes, 64);
        assert!(spec.env.is_none() && spec.timeout.is_none());
    }

    #[test]
    fn validate_accepts_a_good_spec() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = SandboxSpec::new(dir.path(), [program(), "arg"]);
        spec.read.push(dir.path().to_path_buf());
        spec.write.push(dir.path().to_path_buf());
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_bad_commands() {
        let dir = tempfile::tempdir().unwrap();
        let empty: [&str; 0] = [];
        assert_eq!(
            SandboxSpec::new(dir.path(), empty).validate(),
            Err(SpecError::EmptyCommand)
        );
        assert_eq!(
            SandboxSpec::new(dir.path(), [""]).validate(),
            Err(SpecError::EmptyCommand)
        );
        assert_eq!(
            SandboxSpec::new(dir.path(), ["a", "b\0c"]).validate(),
            Err(SpecError::NulInArgument)
        );
        for bat in ["run.bat", "RUN.CMD", r"C:\x\y.Bat", "trailing.cmd. "] {
            assert!(
                matches!(
                    SandboxSpec::new(dir.path(), [bat]).validate(),
                    Err(SpecError::BatchFile(_))
                ),
                "{bat}"
            );
        }
        assert_eq!(SandboxSpec::new(dir.path(), ["batman"]).validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_bad_paths() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(
            SandboxSpec::new(&missing, [program()]).validate(),
            Err(SpecError::CwdNotADirectory(missing.clone()))
        );
        let mut spec = SandboxSpec::new(dir.path(), [program()]);
        spec.read = vec![missing.clone()];
        assert_eq!(spec.validate(), Err(SpecError::MissingPath(missing)));
        spec.read = vec![PathBuf::from("relative/dir")];
        assert_eq!(
            spec.validate(),
            Err(SpecError::RelativePath(PathBuf::from("relative/dir")))
        );
        let root = dir.path().ancestors().last().unwrap().to_path_buf();
        spec.read = vec![];
        spec.write = vec![root.clone()];
        assert_eq!(spec.validate(), Err(SpecError::RootPath(root)));
    }

    #[test]
    fn validate_rejects_bad_environment() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = SandboxSpec::new(dir.path(), [program()]);
        spec.env = Some(vec![("A=B".into(), "x".into())]);
        assert!(matches!(spec.validate(), Err(SpecError::BadEnvironment(_))));
        spec.env = Some(vec![("".into(), "x".into())]);
        assert!(matches!(spec.validate(), Err(SpecError::BadEnvironment(_))));
        spec.env = Some(vec![
            ("=C:".into(), "C:\\x".into()),
            ("A".into(), "b=c".into()),
        ]);
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn effective_dirs_dedupes_and_prefers_write() {
        let mut spec = SandboxSpec::new(".", ["x"]);
        spec.read = vec!["/a".into(), "/b".into(), "/a".into(), "/c".into()];
        spec.write = vec!["/c".into(), "/d".into(), "/d".into()];
        let (read, write) = spec.effective_dirs();
        assert_eq!(read, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(write, vec![PathBuf::from("/c"), PathBuf::from("/d")]);
    }

    #[test]
    fn native_isolation_only_on_windows() {
        assert_eq!(is_native_isolation(), cfg!(windows));
    }
}
