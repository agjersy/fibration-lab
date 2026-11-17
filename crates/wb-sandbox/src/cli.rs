//! Command line of the `present-sandbox` helper.
//!
//! ```text
//! present-sandbox --cwd <dir> --memory <bytes> --processes <n> [--read <dir>]... [--write <dir>]... -- <cmd> [args...]
//! ```
//!
//! This is the contract with `helper_argv` in `python/kernel/present_kernel/runner/sandbox.py`.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::{SandboxSpec, DEFAULT_MEMORY, DEFAULT_PROCESSES};

pub const USAGE: &str = "\
usage: present-sandbox --cwd <dir> [--memory <bytes>] [--processes <n>]
                       [--read <dir>]... [--write <dir>]... [--enter <dir>]...
                       -- <cmd> [args...]

Runs <cmd> in the code sandbox with the standard streams and the environment inherited,
and exits with its exit code (125 if the helper itself fails).

  --cwd <dir>        working directory of the command (default: the current directory)
  --memory <bytes>   memory cap; 0 for none (default: 2147483648)
  --processes <n>    live-process cap; 0 for none (default: 64)
  --read <dir>       grant read and execute access (repeatable)
  --write <dir>      grant modify access (repeatable)
  --enter <dir>      allow entering and listing this directory only, not its contents (repeatable)
  -h, --help         print this text
  -V, --version      print the version
";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    Run(SandboxSpec),
    Help,
    Version,
}

/// A command line that could not be understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn err<T>(message: impl Into<String>) -> Result<T, UsageError> {
    Err(UsageError(message.into()))
}

/// Parse the arguments after the program name.
///
/// Options are only recognised before the first `--`; everything after it is the command,
/// verbatim. A missing `--cwd` means the current directory (resolved by the caller's process).
pub fn parse<I>(args: I) -> Result<Cli, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut cwd: Option<PathBuf> = None;
    let mut memory = DEFAULT_MEMORY;
    let mut processes = DEFAULT_PROCESSES;
    let mut read = Vec::new();
    let mut write = Vec::new();
    let mut enter = Vec::new();
    let mut command: Option<Vec<OsString>> = None;

    while let Some(arg) = args.next() {
        let Some(name) = arg.to_str() else {
            return err(format!("unexpected argument: {}", arg.to_string_lossy()));
        };
        let mut value = |name: &str| -> Result<OsString, UsageError> {
            match args.next() {
                Some(v) if !v.is_empty() => Ok(v),
                _ => err(format!("{name} needs a value")),
            }
        };
        match name {
            "--" => {
                command = Some(args.collect());
                break;
            }
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "--cwd" => cwd = Some(PathBuf::from(value(name)?)),
            "--read" => read.push(PathBuf::from(value(name)?)),
            "--write" => write.push(PathBuf::from(value(name)?)),
            "--enter" => enter.push(PathBuf::from(value(name)?)),
            "--memory" => memory = number(name, &value(name)?)?,
            "--processes" => {
                let n: u64 = number(name, &value(name)?)?;
                processes =
                    u32::try_from(n).or_else(|_| err(format!("{name}: {n} is too large")))?;
            }
            _ if name.starts_with('-') => return err(format!("unknown option: {name}")),
            _ => {
                return err(format!(
                    "unexpected argument: {name} (the command goes after --)"
                ))
            }
        }
    }

    let command = match command {
        Some(c) if !c.is_empty() && !c[0].is_empty() => c,
        Some(_) => return err("no command after --"),
        None => return err("missing -- <cmd>"),
    };
    Ok(Cli::Run(SandboxSpec {
        cwd: cwd.unwrap_or_else(|| PathBuf::from(".")),
        read,
        write,
        enter,
        memory,
        processes,
        command,
        env: None,
        timeout: None,
    }))
}

fn number(name: &str, value: &OsString) -> Result<u64, UsageError> {
    let text = value.to_string_lossy();
    match text.parse::<u64>() {
        Ok(n) => Ok(n),
        Err(_) => err(format!("{name}: not a non-negative integer: {text}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Cli, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    fn spec(args: &[&str]) -> SandboxSpec {
        match parse_strs(args) {
            Ok(Cli::Run(spec)) => spec,
            other => panic!("expected a run, got {other:?}"),
        }
    }

    #[test]
    fn kernel_command_line() {
        // Exactly what helper_argv() in the Python kernel produces.
        let s = spec(&[
            "--cwd",
            r"C:\scratch",
            "--memory",
            "2147483648",
            "--processes",
            "64",
            "--read",
            r"C:\rt",
            "--read",
            r"C:\proj\template",
            "--write",
            r"C:\scratch",
            "--enter",
            r"C:\proj",
            "--",
            r"C:\rt\python.exe",
            "-I",
            "-m",
            "present_kernel.runner",
            "job.json",
        ]);
        assert_eq!(s.cwd, PathBuf::from(r"C:\scratch"));
        assert_eq!(s.memory, 2147483648);
        assert_eq!(s.processes, 64);
        assert_eq!(
            s.read,
            vec![PathBuf::from(r"C:\rt"), PathBuf::from(r"C:\proj\template")]
        );
        assert_eq!(s.write, vec![PathBuf::from(r"C:\scratch")]);
        assert_eq!(s.command.len(), 5);
        assert_eq!(s.command[0], OsString::from(r"C:\rt\python.exe"));
        assert!(s.env.is_none() && s.timeout.is_none());
    }

    #[test]
    fn defaults_apply() {
        let s = spec(&["--", "prog"]);
        assert_eq!(s.cwd, PathBuf::from("."));
        assert_eq!(s.memory, DEFAULT_MEMORY);
        assert_eq!(s.processes, DEFAULT_PROCESSES);
        assert!(s.read.is_empty() && s.write.is_empty());
    }

    #[test]
    fn everything_after_the_separator_is_the_command() {
        let s = spec(&[
            "--cwd", "d", "--", "prog", "--read", "x", "--", "--help", "",
        ]);
        let expected: Vec<OsString> = ["prog", "--read", "x", "--", "--help", ""]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(s.command, expected);
        assert!(s.read.is_empty());
    }

    #[test]
    fn zero_disables_limits() {
        let s = spec(&["--memory", "0", "--processes", "0", "--", "p"]);
        assert_eq!((s.memory, s.processes), (0, 0));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse_strs(&["--help"]), Ok(Cli::Help));
        assert_eq!(parse_strs(&["-h"]), Ok(Cli::Help));
        assert_eq!(parse_strs(&["--version"]), Ok(Cli::Version));
        assert_eq!(parse_strs(&["--cwd", "x", "-V"]), Ok(Cli::Version));
    }

    #[test]
    fn usage_errors() {
        for args in [
            &[][..],
            &["--cwd", "x"],
            &["--"],
            &["--", ""],
            &["--cwd"],
            &["--cwd", "", "--", "p"],
            &["--memory", "lots", "--", "p"],
            &["--memory", "-1", "--", "p"],
            &["--memory", "1.5", "--", "p"],
            &["--processes", "4294967296", "--", "p"],
            &["--network", "--", "p"],
            &["prog", "arg"],
            &["--read"],
        ] {
            assert!(parse_strs(args).is_err(), "{args:?} should be rejected");
        }
    }

    #[test]
    fn option_values_may_look_like_options() {
        let s = spec(&["--read", "--odd-dir", "--", "p"]);
        assert_eq!(s.read, vec![PathBuf::from("--odd-dir")]);
    }
}
