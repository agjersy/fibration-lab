//! Non-Windows behaviour of the `present-sandbox` binary and the library: the command runs
//! directly (no isolation), streams and environment pass through, exit codes propagate.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use wb_sandbox::{is_native_isolation, run_sandboxed, SandboxSpec, EXIT_HELPER_FAILURE};

const HELPER: &str = env!("CARGO_BIN_EXE_present-sandbox");
const PROBE: &str = env!("CARGO_BIN_EXE_sandbox-probe");

fn helper(dir: &Path, command: &[&str]) -> std::process::Output {
    Command::new(HELPER)
        .args([
            "--cwd",
            dir.to_str().unwrap(),
            "--memory",
            "0",
            "--processes",
            "0",
            "--",
        ])
        .args(command)
        .output()
        .unwrap()
}

#[test]
fn runs_echo_and_warns_that_nothing_is_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let out = helper(dir.path(), &["/bin/echo", "hello", "two words"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello two words\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("no isolation"), "{stderr}");
    assert!(!is_native_isolation());
}

#[test]
fn propagates_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        helper(dir.path(), &["/bin/sh", "-c", "exit 0"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        helper(dir.path(), &["/bin/sh", "-c", "exit 3"])
            .status
            .code(),
        Some(3)
    );
    assert_eq!(
        helper(dir.path(), &[PROBE, "exit", "42"]).status.code(),
        Some(42)
    );
}

#[test]
fn passes_arguments_environment_and_cwd_through() {
    let dir = tempfile::tempdir().unwrap();
    let out = helper(dir.path(), &[PROBE, "echo", "a b", "", "--read", "--"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b||--read|--\n");

    let out = Command::new(HELPER)
        .args([
            "--cwd",
            dir.path().to_str().unwrap(),
            "--",
            PROBE,
            "env",
            "WB_SANDBOX_TEST_VALUE",
        ])
        .env("WB_SANDBOX_TEST_VALUE", "passed through")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "passed through\n");

    std::fs::write(dir.path().join("here.txt"), "found via cwd").unwrap();
    let out = helper(dir.path(), &[PROBE, "read", "here.txt"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "found via cwd");
}

#[test]
fn helper_failures_exit_125() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(HELPER)
        .args(["--cwd", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(EXIT_HELPER_FAILURE));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage:"));

    let out = helper(dir.path(), &["/nonexistent/program"]);
    assert_eq!(out.status.code(), Some(EXIT_HELPER_FAILURE));

    let missing = dir.path().join("missing");
    let out = Command::new(HELPER)
        .args(["--cwd", missing.to_str().unwrap(), "--", "/bin/echo"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(EXIT_HELPER_FAILURE));
    assert!(out.stdout.is_empty());
}

#[test]
fn help_and_version() {
    let out = Command::new(HELPER).arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("usage: present-sandbox"));
    let out = Command::new(HELPER).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("present-sandbox "));
}

#[test]
fn library_captures_output_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = SandboxSpec::new(
        dir.path(),
        ["/bin/sh", "-c", "echo out; echo err >&2; exit 5"],
    );
    spec.processes = 0;
    let out = run_sandboxed(&spec).unwrap();
    assert_eq!(out.exit_code, 5);
    assert_eq!(out.stdout, b"out\n");
    assert_eq!(out.stderr, b"err\n");
    assert!(!out.timed_out && !out.success());
}

#[test]
fn library_replaces_the_environment_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = SandboxSpec::new(dir.path(), [PROBE, "env", "ONLY"]);
    spec.env = Some(vec![("ONLY".into(), "this".into())]);
    assert_eq!(run_sandboxed(&spec).unwrap().stdout, b"this\n");
    spec.command = vec![PROBE.into(), "env".into(), "PATH".into()];
    assert_eq!(run_sandboxed(&spec).unwrap().exit_code, 1);
}

#[test]
fn library_enforces_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = SandboxSpec::new(dir.path(), [PROBE, "sleep", "30000"]);
    spec.timeout = Some(Duration::from_millis(300));
    let started = Instant::now();
    let out = run_sandboxed(&spec).unwrap();
    assert!(out.timed_out);
    assert_ne!(out.exit_code, 0);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn library_rejects_invalid_specs_before_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = SandboxSpec::new(dir.path(), [PROBE, "exit", "0"]);
    spec.read.push(dir.path().join("missing"));
    let err = run_sandboxed(&spec).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

/// `RLIMIT_AS` is honoured on Linux (macOS does not enforce it).
#[cfg(target_os = "linux")]
#[test]
fn memory_cap_stops_a_hog_on_linux() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = SandboxSpec::new(dir.path(), [PROBE, "hog", "2048"]);
    spec.memory = 512 * 1024 * 1024;
    spec.processes = 0;
    spec.timeout = Some(Duration::from_secs(60));
    let out = run_sandboxed(&spec).unwrap();
    assert_ne!(out.exit_code, 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("hog-survived"));

    spec.command = vec![PROBE.into(), "hog".into(), "32".into()];
    let out = run_sandboxed(&spec).unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hog-survived"),
        "{out:?}"
    );
}
