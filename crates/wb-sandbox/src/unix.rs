//! Non-Windows path: no isolation, resource limits only.
//!
//! The Python kernel wraps `sandbox-exec` (macOS) and `bwrap` (Linux) itself and never launches
//! this helper on those systems; this exists so the crate and binary build and behave sensibly
//! everywhere.

use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio as ProcStdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::{Output, SandboxSpec, Stdio};

/// Best-effort limits, applied in the forked child before `exec`. Failures are ignored: macOS
/// rejects or does not enforce `RLIMIT_AS`, and a limit above the hard limit cannot be set.
///
/// `RLIMIT_NPROC` counts every process of the user, not just the sandbox's, so a child that
/// forks may fail on a busy account; it is ignored for root.
fn apply_limits(memory: u64, processes: u32) {
    // Only async-signal-safe calls are allowed here.
    unsafe {
        if memory > 0 {
            let value = memory as libc::rlim_t;
            let limit = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            let _ = libc::setrlimit(libc::RLIMIT_AS, &limit);
        }
        if processes > 0 {
            let value = processes as libc::rlim_t;
            let limit = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            let _ = libc::setrlimit(libc::RLIMIT_NPROC, &limit);
        }
    }
}

fn drain(stream: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stream {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    })
}

pub(crate) fn run(spec: &SandboxSpec, stdio: Stdio) -> io::Result<Output> {
    let mut cmd = Command::new(&spec.command[0]);
    cmd.args(&spec.command[1..]).current_dir(&spec.cwd);
    if let Some(env) = &spec.env {
        cmd.env_clear();
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
    }
    if stdio == Stdio::Capture {
        cmd.stdin(ProcStdio::null())
            .stdout(ProcStdio::piped())
            .stderr(ProcStdio::piped());
    }
    let (memory, processes) = (spec.memory, spec.processes);
    // SAFETY: the closure only calls setrlimit, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            apply_limits(memory, processes);
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let mut timed_out = false;
    let status = match spec.timeout {
        None => child.wait()?,
        Some(timeout) => {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait()?;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let exit_code = status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(-1);
    Ok(Output {
        exit_code,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
        timed_out,
    })
}
