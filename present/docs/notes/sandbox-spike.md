# Sandbox spike (S1): Windows AppContainer helper

*Status: implemented and type-checked; **the Windows path has not been executed yet.** It was written on Linux,
where it compiles and passes clippy for `x86_64-pc-windows-gnu` and `x86_64-pc-windows-msvc`, but no Windows
machine has run it. Everything under "Findings to fill in" is unknown until the `windows-latest` CI job runs
`crates/wb-sandbox/tests/windows.rs`.*

Contracts: PROTOCOL.md section 9, ARCHITECTURE.md section 12.1, BUILD-SPEC S1.

## 1. What exists

| Piece | Where | Role |
|---|---|---|
| `present-sandbox` binary | `crates/wb-sandbox/src/bin/present-sandbox.rs` | The helper the Python kernel launches on Windows |
| Library | `crates/wb-sandbox/src/lib.rs` | `SandboxSpec`, `run_sandboxed` (captured output, optional timeout), `run_inherited` (inherited streams, returns the exit code), `is_native_isolation()` |
| Windows implementation | `src/windows.rs` | AppContainer, ACEs, job object, process creation |
| Fallback | `src/unix.rs` | No isolation: runs the command with `RLIMIT_AS` / `RLIMIT_NPROC`, one warning line on stderr |
| Quoting | `src/quote.rs` | `CommandLineToArgvW` rules, pure, tested on every platform |
| Journal | `src/journal.rs` | Record of a run's on-disk changes, for cleanup after a killed helper |
| `sandbox-probe` binary | `src/bin/sandbox-probe.rs` | Test-only child (read, write, connect, allocate). Not shipped |

macOS (`sandbox-exec`) and Linux (`bwrap`) isolation stays in the Python kernel
(`python/kernel/present_kernel/runner/sandbox.py`); the kernel uses this crate only on Windows. Porting the
macOS profile generator into this crate, as BUILD-SPEC S1 words it, is not done here.

Command line (the contract with `helper_argv` in the kernel):

```
present-sandbox --cwd <dir> --memory <bytes> --processes <n> [--read <dir>]... [--write <dir>]... -- <cmd> [args...]
```

The child inherits the helper's stdin, stdout, stderr, and environment. The helper exits with the child's exit
code, or 125 when the helper itself fails (bad usage, sandbox setup failed, the command could not be started).
`--memory 0` and `--processes 0` disable the respective limit. Dependencies: `windows-sys` (MIT OR Apache-2.0)
on Windows, `libc` (MIT OR Apache-2.0) on Unix, `tempfile` for tests.

## 2. Design of a Windows run

1. **Lock.** Take the named mutex `Local\wb-sandbox-acl`. Editing a DACL is read-modify-write; two helpers
   granting on the same directory at once could lose one another's entry.
2. **Clean up dead runs.** For every journal in `%LOCALAPPDATA%\wb-sandbox\journal` that can be opened without
   sharing (so its owner is dead): revoke its SID from its paths, delete its profile, delete the journal.
3. **Profile.** `CreateAppContainerProfile` with a unique name `present.sandbox.<pid>.<counter>.<clock>` and
   **no capabilities**. No `internetClient`, `internetClientServer`, or `privateNetworkClientServer` means no
   outbound or inbound sockets, loopback included. If the name already exists the SID is taken from
   `DeriveAppContainerSidFromAppContainerName`.
4. **Journal.** Write `<name>.journal` (profile name, SID string, paths about to be changed), flush it, and
   keep it open with share mode 0 for the life of the run.
5. **Access entries.** For each `--read` path: an allow ACE for the AppContainer SID with
   `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`; for each `--write` path: "Modify"
   (`FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE`). Directories get
   `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`. Calls: `GetNamedSecurityInfoW`, `SetEntriesInAclW`,
   `SetNamedSecurityInfoW`. A path listed in both sets gets only the write entry. A `--read` directory whose own
   DACL already gives ALL APPLICATION PACKAGES inheritable read and execute is skipped (see 4.1).
6. **Unlock**, then create the job object: `JOB_OBJECT_LIMIT_PROCESS_MEMORY` and `JOB_OBJECT_LIMIT_JOB_MEMORY`
   from `--memory`, `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` from `--processes`, `KILL_ON_JOB_CLOSE`,
   `DIE_ON_UNHANDLED_EXCEPTION`, and UI restrictions (desktop, display settings, exit windows, global atoms,
   handles, read clipboard, write clipboard, system parameters).
7. **Start the child** with `CreateProcessW` and `STARTUPINFOEXW`: `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
   (the AppContainer SID, zero capabilities) and `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` restricted to the three
   standard handles, flags `EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT`.
   Assign it to the job, `ResumeThread`, wait, `GetExitCodeProcess`.
8. **Tear down.** `TerminateJobObject` for anything the child left running, retake the mutex, revoke the SID
   from every path (`REVOKE_ACCESS`), `DeleteAppContainerProfile`, delete the journal. This runs from `Drop`,
   so it also happens when a step above fails.

Details worth knowing:

- **Handle list.** The helper is started by the kernel with handle inheritance on, so it may hold inheritable
  handles that have nothing to do with the child. The explicit list keeps them out of the sandbox.
- **Quoting.** Arguments are quoted by the `CommandLineToArgvW` rules; `argv[0]` is always wrapped in plain
  quotes because the parsers do not treat backslashes as escapes there. `.bat` and `.cmd` programs are refused:
  `CreateProcessW` would hand them to `cmd.exe`, which parses its command line by different rules.
- **Program lookup.** `lpApplicationName` is NULL, so a bare name such as `cmd.exe` is searched the way
  `CreateProcessW` does (the helper's directory, the current directory, system directories, the helper's
  `PATH`). The kernel passes an absolute interpreter path.
- **Memory cap behaviour.** A job memory limit does not kill the process when it is reached; further commits
  fail. Python raises `MemoryError`, native code usually aborts. "Killed at the cap" in S1 (e) is therefore
  tested as "exits non-zero without completing the allocation".
- **Console events.** The helper installs a console control handler that swallows Ctrl+C and Ctrl+Break for
  itself. The child shares the console and still receives them; the helper then cleans up and exits.
- **No ancestors are touched.** AppContainer tokens keep the bypass-traverse-checking privilege, so entries
  are needed only on the listed paths, not on their parent directories.

## 3. The DACL mutation, stated plainly

For the duration of a run the helper **adds an allow ACE to the DACL of every `--read` and `--write` path**, and
because the ACE is inheritable Windows rewrites the DACL of every file and folder below it. After the run the
ACE is removed again, which rewrites them a second time. Consequences:

- **Other software can observe the change** while a run is in progress (backup tools, sync clients, file
  integrity monitors, `icacls`). The entry names a SID that only the sandboxed process holds, so it gives no
  other principal any access.
- **Cost grows with the number of files** under the listed directories, twice per run. See 4.1.
- **If the helper is killed** (the kernel's timeout path runs `taskkill /F /T` on it; also a crash or power
  loss) the job object closes and the child tree dies with it, but the ACEs and the profile remain. The entries
  are harmless in the sense that nothing else can hold that SID, though once the profile is gone they show as
  an "unknown account" SID in the Security tab. The next helper run finds the dead run's journal and removes
  both. Nothing cleans them if the helper is never run again.
- **Cleanup limits.** An AppContainer SID is a hash of the profile name and cannot be mapped back to it, so
  stale entries cannot be found by scanning DACLs for "our prefix"; the journal is the only record. If the
  journal could not be written (the helper warns on stderr and continues) a killed run leaves entries that are
  never cleaned automatically. A journal is only honoured if its SID has the AppContainer shape (`S-1-15-2-...`
  with at least two sub-authorities) and its profile name has our prefix, so a damaged or planted journal cannot
  strip a user's or group's entries. Journals whose revocation fails are kept and retried.
- **The caller needs `WRITE_DAC`** on each listed path, which the owner has. A read-only install location the
  user does not own (Program Files) cannot be granted to; it normally does not need to be (4.1).
- **Objects with a NULL DACL are refused** rather than given a DACL containing only our entry, which would lock
  everyone else out.
- **Filesystem roots are refused** as `--read`/`--write` paths, so a mistake cannot rewrite a whole volume.

## 4. Known risks

### 4.1 Launch latency (unknown until measured)

Nothing here has been timed. The expected cost centres are `CreateAppContainerProfile` /
`DeleteAppContainerProfile` (registry and a folder under `%LOCALAPPDATA%\Packages`) and, above all, ACE
propagation over the bundled Python runtime, which holds thousands of files and is granted and revoked on
every launch. Section 6 says how to measure. If propagation dominates, the options in order of preference:

1. Have the installer (or first run) put a permanent inheritable read-and-execute ACE for ALL APPLICATION
   PACKAGES on the runtime directory. The helper already detects that and skips the directory. Cost: every
   AppContainer app of that user can read the runtime, which holds nothing secret.
2. Use one long-lived profile per installation and leave the runtime entry in place between runs, with per-run
   entries only for the small project and scratch directories.

The check in option 1 looks only at the directory's own DACL, not at every file below it. A file under such a
directory with inheritance disabled would be unreadable in the sandbox and the run would fail visibly.

### 4.2 AppContainer availability

`CreateAppContainerProfile` can be unavailable or blocked: enterprise policy, a profile that is not fully
loaded (some service accounts and `runas /noprofile` sessions), roaming or redirected `%LOCALAPPDATA%`. The
helper then exits 125 with the error on stderr. **The kernel currently treats the presence of the helper file
as "AppContainer available"** (`detect()` in `sandbox.py`); it does not probe. A machine where profiles cannot
be created would therefore fail every build instead of falling back to `limits`. A probe at `kernel.hello`
(run the helper with a trivial command, cache the result) is the fix and belongs to the kernel work package.

### 4.3 Standard handles and the console

Expected to work, not yet observed: pipes and the NUL device inherited from the kernel are usable inside the
AppContainer (inherited handles are not re-checked against the child's token). When the helper is run by hand
in a console, the child inherits the console handles; AppContainer processes attaching to an existing console
works on current Windows 10 and 11 but is the least certain case. If a standard handle cannot be made
inheritable it is passed as "no handle" rather than failing the launch.

### 4.4 Nested jobs and UI restrictions

If the helper already runs inside a job (some CI runners, some launchers), the child joins a nested job, and
Windows is understood not to allow UI restrictions in a nested job hierarchy (to be confirmed). When assigning the child to the fully restricted job
fails, the helper retries with a job that has the memory, process, and kill-on-close limits but no UI
restrictions, and prints a warning. The AppContainer boundary is unaffected.

### 4.5 What the AppContainer may still reach

- Everything ALL APPLICATION PACKAGES can read: the Windows directory, Program Files, and whatever other
  software has opened up. That is the platform's baseline and matches "read everywhere outside home" on macOS.
- Its own profile folder under `%LOCALAPPDATA%\Packages\<name>` (created and deleted with the profile). Windows
  may also point `TEMP`, `TMP`, and `LOCALAPPDATA` of an AppContainer process there regardless of the
  environment passed in. If so, temporary files land in the profile folder instead of scratch; they are deleted
  with the profile. To be confirmed on Windows by printing those variables from inside the sandbox.
- Files written by the child inherit the scratch directory's entries, and lose the AppContainer entry when it
  is revoked.

### 4.6 Other limits of this spike

- The working directory must lie under a `--read` or `--write` path (the kernel uses the scratch directory).
- Paths that are not valid Unicode are journalled lossily; such a path would not be cleaned after a kill.
- CPU time is not limited by the job; the kernel enforces the wall-clock timeout by killing the helper.
- The library's `timeout` kills the job and reports `timed_out`; the binary has no timeout option.
- `RLIMIT_NPROC` on the Unix fallback counts all of the user's processes, so a forking child can fail on a busy
  account. The fallback is not used by the kernel.
- Windows on ARM64 has not been compiled or checked.

## 5. What is verified where

| Check | Where it ran | Result |
|---|---|---|
| `cargo fmt -p wb-sandbox -- --check` | Linux | pass |
| `cargo clippy -p wb-sandbox --all-targets -- -D warnings` | Linux | pass |
| `cargo test -p wb-sandbox`: CLI parsing, quoting (including a round trip through a reference argv parser), spec validation, journal format | Linux | pass |
| `tests/unix.rs`: the binary runs `echo`, propagates exit codes, arguments, environment, cwd; exits 125 on helper failure; library capture, timeout, `RLIMIT_AS` stopping a memory hog | Linux | pass |
| `cargo clippy -p wb-sandbox --all-targets --target x86_64-pc-windows-gnu -- -D warnings` and the same for `x86_64-pc-windows-msvc` | Linux, cross type-check only | pass |
| Linking a Windows binary | nowhere | not done |
| `tests/windows.rs`, S1 (a) to (e) | `windows-latest` CI | **not run yet** |
| S1 (f), Python imports inside the sandbox | Python suite on Windows | **not run yet** |
| macOS | CI | the Unix tests should pass there; not run |

`tests/windows.rs` covers: (a) reading an allowed file, with the probe and with `cmd /c type`; (b) reading a
file in a directory under the user profile that was not granted fails, with a control read outside the sandbox;
(c) writing to scratch works, writing to an ungranted directory and to a read-only directory fails; (d)
connecting to `1.1.1.1:443` fails, and connecting to a loopback listener fails inside the sandbox while
succeeding outside it; (e) a 1 GiB allocation under a 256 MiB cap fails while 32 MiB succeeds; plus timeout,
argument quoting, explicit environment, removal of the entries after a run (via `icacls`), and the helper
binary's contract (streams, exit code).

Assumptions in those tests that the first CI run will confirm or refute: a Rust executable copied into a
granted directory starts inside the AppContainer (its runtime DLLs come from System32); `cmd.exe` starts
inside it; `icacls` prints a removed profile's entries as raw `S-1-15-2-...` SIDs.

## 6. Measuring latency on Windows

Set `WB_SANDBOX_TRACE=1` and the helper prints one line of phase timings to stderr after the run:

```
present-sandbox: trace: setup 41 ms (stale cleanup, profile, access entries), child 95 ms, teardown 38 ms
```

(The numbers above are placeholders showing the format, not measurements.) A measurement session in
PowerShell, from the repository root after `cargo build --release -p wb-sandbox`:

```powershell
$helper  = "target\release\present-sandbox.exe"
$runtime = "<path to the bundled Python runtime>"
$scratch = New-Item -ItemType Directory "$env:USERPROFILE\sandbox-latency-scratch" -Force
$env:WB_SANDBOX_TRACE = "1"

# 1. Fixed overhead: profile + job + process, only the small scratch directory granted.
1..10 | % { (Measure-Command { & $helper --cwd $scratch --write $scratch -- cmd.exe /d /c exit 0 }).TotalMilliseconds }

# 2. With the runtime granted: adds ACE propagation over every runtime file, twice.
1..10 | % { (Measure-Command { & $helper --cwd $scratch --read $runtime --write $scratch -- "$runtime\python.exe" -I -c "pass" }).TotalMilliseconds }

# 3. Baseline: the same interpreter start without the helper.
1..10 | % { (Measure-Command { & "$runtime\python.exe" -I -c "pass" }).TotalMilliseconds }

# 4. Option 1 of section 4.1: a permanent entry, then repeat step 2 and compare.
icacls $runtime /grant "*S-1-15-2-1:(OI)(CI)(RX)"
```

Record medians for each step, the file count of the runtime (`(Get-ChildItem $runtime -Recurse -File).Count`),
the Windows build, and whether real-time antivirus scanning was on. Step 2 minus step 3 is the per-launch cost
the user pays on every build; it should be well under the 60 s build timeout and ideally under half a second.

## 7. Findings to fill in after the first Windows run

- Do all of `tests/windows.rs` pass on `windows-latest`? Which assumptions in section 5 were wrong?
- Median launch overhead for steps 1, 2, and 4 above.
- Does the runner place the helper in a job (is the UI-restriction warning printed)?
- Values of `TEMP`, `TMP`, `LOCALAPPDATA` seen inside the sandbox.
- Does the bundled Python import `pptx`, `pandas`, `matplotlib` inside the sandbox (S1 (f))?
