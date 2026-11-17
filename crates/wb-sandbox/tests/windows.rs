//! BUILD-SPEC S1 acceptance (a)-(e) for the Windows AppContainer sandbox. Runs on the
//! `windows-latest` CI runner. (f), the Python import test, lives in the Python suite.
//!
//! The child is `sandbox-probe`, a small binary of this crate, copied into a directory the
//! sandbox may read; `cmd.exe` is used once to show that a system program starts too. Every
//! denial is paired with a control showing the same action works outside the sandbox (or inside
//! it on an allowed path), so a test cannot pass merely because the probe is broken.
#![cfg(windows)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use wb_sandbox::{is_native_isolation, run_sandboxed, Output, SandboxSpec};

const HELPER: &str = env!("CARGO_BIN_EXE_present-sandbox");
const PROBE: &str = env!("CARGO_BIN_EXE_sandbox-probe");
const TIMEOUT: Duration = Duration::from_secs(30);
const MIB: u64 = 1024 * 1024;

/// A temporary directory under the user's profile, where AppContainers have no access by default.
fn profile_tempdir(tag: &str) -> TempDir {
    let home = PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE is set"));
    tempfile::Builder::new()
        .prefix(&format!("wb-sandbox-test-{tag}-"))
        .tempdir_in(home)
        .unwrap()
}

struct Fixture {
    /// Readable by the sandbox: holds the probe and `allowed.txt`.
    allowed: TempDir,
    /// Writable by the sandbox; also its working directory.
    scratch: TempDir,
    /// Not granted.
    private: TempDir,
    probe: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let allowed = profile_tempdir("allowed");
        let scratch = profile_tempdir("scratch");
        let private = profile_tempdir("private");
        let probe = allowed.path().join("sandbox-probe.exe");
        fs::copy(PROBE, &probe).unwrap();
        fs::write(allowed.path().join("allowed.txt"), "allowed content").unwrap();
        fs::write(private.path().join("secret.txt"), "secret content").unwrap();
        Fixture {
            allowed,
            scratch,
            private,
            probe,
        }
    }

    fn spec(&self, args: &[&str]) -> SandboxSpec {
        let mut command = vec![self.probe.clone().into_os_string()];
        command.extend(args.iter().map(Into::into));
        let mut spec = SandboxSpec::new(self.scratch.path(), command);
        spec.read = vec![self.allowed.path().to_path_buf()];
        spec.write = vec![self.scratch.path().to_path_buf()];
        spec.processes = 8;
        spec.timeout = Some(TIMEOUT);
        spec
    }

    fn sandboxed(&self, args: &[&str]) -> Output {
        let out = run_sandboxed(&self.spec(args)).expect("the sandbox starts");
        assert!(!out.timed_out, "timed out: {args:?}");
        out
    }

    /// The same probe, outside the sandbox.
    fn unsandboxed(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.probe)
            .args(args)
            .current_dir(self.scratch.path())
            .output()
            .unwrap()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("test paths are valid Unicode")
}

#[test]
fn reports_native_isolation() {
    assert!(is_native_isolation());
}

/// (a) Reading an allowed file succeeds.
#[test]
fn a_reading_an_allowed_file_succeeds() {
    let fx = Fixture::new();
    let file = fx.allowed.path().join("allowed.txt");
    let out = fx.sandboxed(&["read", path_str(&file)]);
    assert_eq!(out.exit_code, 0, "stderr: {}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "allowed content");
}

/// (a) again with a system program, to show that binaries outside the granted set start.
#[test]
fn a_cmd_can_type_an_allowed_file() {
    let fx = Fixture::new();
    let file = fx.allowed.path().join("allowed.txt");
    let mut spec = fx.spec(&[]);
    spec.command = vec![
        "cmd.exe".into(),
        "/d".into(),
        "/c".into(),
        "type".into(),
        file.into_os_string(),
    ];
    let out = run_sandboxed(&spec).expect("the sandbox starts");
    assert_eq!(out.exit_code, 0, "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).contains("allowed content"));
}

/// (b) Reading a file in the user's profile outside the allowed set fails.
#[test]
fn b_reading_outside_the_allowed_set_fails() {
    let fx = Fixture::new();
    let secret = fx.private.path().join("secret.txt");
    let control = fx.unsandboxed(&["read", path_str(&secret)]);
    assert_eq!(
        text(&control.stdout),
        "secret content",
        "control: the file is readable outside the sandbox"
    );

    let out = fx.sandboxed(&["read", path_str(&secret)]);
    assert_eq!(
        out.exit_code,
        1,
        "stdout: {} stderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert!(!text(&out.stdout).contains("secret"));
    assert!(text(&out.stderr).contains("read-failed"));
}

/// (c) Writing outside the scratch directory fails; writing inside it works.
#[test]
fn c_writing_outside_scratch_fails() {
    let fx = Fixture::new();
    let inside = fx.scratch.path().join("out.txt");
    let out = fx.sandboxed(&["write", path_str(&inside)]);
    assert_eq!(
        out.exit_code,
        0,
        "control: scratch is writable; stderr: {}",
        text(&out.stderr)
    );
    assert!(inside.is_file());

    let private = fx.private.path().join("out.txt");
    let out = fx.sandboxed(&["write", path_str(&private)]);
    assert_eq!(out.exit_code, 1, "stderr: {}", text(&out.stderr));
    assert!(!private.exists());

    let read_only = fx.allowed.path().join("out.txt");
    let out = fx.sandboxed(&["write", path_str(&read_only)]);
    assert_eq!(
        out.exit_code,
        1,
        "read directories are not writable; stderr: {}",
        text(&out.stderr)
    );
    assert!(!read_only.exists());

    let overwrite = fx.allowed.path().join("allowed.txt");
    let out = fx.sandboxed(&["write", path_str(&overwrite)]);
    assert_eq!(out.exit_code, 1);
    assert_eq!(fs::read_to_string(&overwrite).unwrap(), "allowed content");
}

/// (d) An outbound TCP connection to 1.1.1.1:443 fails.
#[test]
fn d_outbound_tcp_fails() {
    let fx = Fixture::new();
    let out = fx.sandboxed(&["connect", "1.1.1.1:443", "5000"]);
    assert_eq!(out.exit_code, 1, "stdout: {}", text(&out.stdout));
    assert!(text(&out.stderr).contains("connect-failed"));
}

/// (d) with a control that does not depend on the runner's internet access: a listener on the
/// loopback interface is reachable outside the sandbox and not inside it.
#[test]
fn d_loopback_tcp_fails() {
    let fx = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let control = fx.unsandboxed(&["connect", &addr, "5000"]);
    assert_eq!(
        control.status.code(),
        Some(0),
        "control: the listener is reachable outside the sandbox"
    );

    let out = fx.sandboxed(&["connect", &addr, "5000"]);
    assert_eq!(out.exit_code, 1, "stdout: {}", text(&out.stdout));
}

/// (e) A memory hog is stopped at the cap. A job memory limit makes allocations fail rather than
/// killing the process outright; the probe (like Python, with `MemoryError`) then exits non-zero.
#[test]
fn e_memory_hog_is_stopped_at_the_cap() {
    let fx = Fixture::new();
    let mut spec = fx.spec(&["hog", "32"]);
    spec.memory = 256 * MIB;
    let out = run_sandboxed(&spec).expect("the sandbox starts");
    assert_eq!(
        out.exit_code,
        0,
        "control: a small allocation fits; stderr: {}",
        text(&out.stderr)
    );
    assert!(text(&out.stdout).contains("hog-survived"));

    let mut spec = fx.spec(&["hog", "1024"]);
    spec.memory = 256 * MIB;
    let out = run_sandboxed(&spec).expect("the sandbox starts");
    assert!(!out.timed_out);
    assert_ne!(out.exit_code, 0);
    assert!(!text(&out.stdout).contains("hog-survived"));
}

#[test]
fn timeout_kills_the_sandbox() {
    let fx = Fixture::new();
    let mut spec = fx.spec(&["sleep", "60000"]);
    spec.timeout = Some(Duration::from_secs(2));
    let started = Instant::now();
    let out = run_sandboxed(&spec).expect("the sandbox starts");
    assert!(out.timed_out);
    assert!(started.elapsed() < TIMEOUT);
}

#[test]
fn arguments_and_environment_reach_the_child() {
    let fx = Fixture::new();
    let tricky = [
        "a b",
        "",
        r#"say "hi""#,
        r"trail\",
        r"C:\dir with space\",
        r#"\"x\\"#,
    ];
    let mut args = vec!["echo"];
    args.extend_from_slice(&tricky);
    let out = fx.sandboxed(&args);
    assert_eq!(
        text(&out.stdout).trim_end_matches(['\r', '\n']),
        tricky.join("|")
    );

    let mut spec = fx.spec(&["env", "WB_SANDBOX_TEST_VALUE"]);
    let mut env: Vec<_> = std::env::vars_os().collect();
    env.push(("WB_SANDBOX_TEST_VALUE".into(), "explicit".into()));
    spec.env = Some(env);
    let out = run_sandboxed(&spec).expect("the sandbox starts");
    assert_eq!(text(&out.stdout).trim_end(), "explicit");
}

/// The access entries added for a run are gone afterwards.
#[test]
fn access_entries_are_removed_after_the_run() {
    let fx = Fixture::new();
    let out = fx.sandboxed(&["exit", "0"]);
    assert_eq!(out.exit_code, 0);
    for dir in [fx.allowed.path(), fx.scratch.path()] {
        let listing = Command::new("icacls").arg(dir).output().unwrap();
        let listing = text(&listing.stdout);
        // An entry for a deleted profile shows as a raw SID; one for a live profile by name.
        assert!(
            !listing.contains("S-1-15-2-"),
            "stale AppContainer entry on {}: {listing}",
            dir.display()
        );
        assert!(
            !listing.contains("present.sandbox"),
            "stale AppContainer entry on {}: {listing}",
            dir.display()
        );
    }
}

/// The helper binary, as the Python kernel invokes it: streams inherited, exit code propagated.
#[test]
fn helper_binary_matches_the_kernel_contract() {
    let fx = Fixture::new();
    let helper = |command: &[&str]| -> std::process::Output {
        let mut child = Command::new(HELPER)
            .args([
                "--cwd",
                path_str(fx.scratch.path()),
                "--memory",
                "536870912",
                "--processes",
                "8",
            ])
            .args([
                "--read",
                path_str(fx.allowed.path()),
                "--write",
                path_str(fx.scratch.path()),
                "--",
            ])
            .arg(&fx.probe)
            .args(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // The helper has no timeout of its own (the kernel enforces one), so guard here.
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                panic!("the helper did not finish: {command:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    };

    let allowed = fx.allowed.path().join("allowed.txt");
    let out = helper(&["read", path_str(&allowed)]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "allowed content");

    assert_eq!(helper(&["exit", "7"]).status.code(), Some(7));

    let secret = fx.private.path().join("secret.txt");
    let out = helper(&["read", path_str(&secret)]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("read-failed"));
}
