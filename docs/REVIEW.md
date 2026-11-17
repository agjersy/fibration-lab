# Design and Implementation Review

*Reviewed 2026-11-15 at commit `f229e4d`; fixes applied 2026-11-17. The findings are kept as written; the resolution section records what was done.*

## Resolution (2026-11-17)

The findings below were fixed on the working branch in four streams (kernel `818773e`, core `e8860ee`, renderer `103478b`, service and shell `b96d3b3`), with the behavior specified first in PROTOCOL.md section 9 and IPC.md section 7. Every finding that had a reproduction now has a regression test.

Verified after the fixes: 190 Rust tests, 165 kernel tests, 104 renderer unit tests, 10 end-to-end specs, the desktop smoke test (now including close and reopen), and the scripted agent-loop evaluation. The `Math:` exploit (G1) was re-run against the fixed kernel under a real `bubblewrap` sandbox and is rejected. A packaged layout with no dev checkout was simulated and bootstraps its own tooling environment.

**Fixed and verified here:** G1, G2, G4, G5, G6, G7, G9, B1 to B13, R1 (against local directories), R7, K1 to K10, C1 to C5, A1 to A11, U1 to U11, and the vacuous-test items.

**Implemented but not executed here**

| Area | Why | Items |
|---|---|---|
| Remote targets over SSH | no SSH server in the build environment | G8, R2 to R6, R8 beyond command construction, prepare-on-open, debounced sync, package flow, remote terminals |
| macOS | Linux build environment | the `sandbox-exec` profile (a test now executes it, and CI runs the kernel suite on macOS), the app menu without Close Window, the FSEvents watcher path, the `.app` resource path |
| Installers | not built | `tauri build` output; only the resource layout was simulated |

**Not changed, by decision or scope**

- G3: on hosts where `bubblewrap` cannot create user namespaces the kernel still falls back to `limits`. What changed is the consequence: runs, snippets, and shell calls now ask first and the app shows an "unsandboxed" badge. A Landlock backend or a documented AppArmor profile is the follow-up.
- The sandbox still allows reads outside the home directory, and per-host network allowlists are not built; ARCHITECTURE 5.6 now says so.
- Language server, `fig.get` for vision models, OS keychain storage, and a SQLite index remain planned; the documents no longer describe them as present.
- `matplotlib.use("Agg")` before pyplot is imported still disables figure capture silently.
- Capability cache entries poisoned by an earlier version are not auto-detected; re-probe or set an override.

## How this was reviewed

Five independent passes: the Python kernel, the Rust agent core (loop, tools, providers, checkpoints), the Rust service layer with targets and the Tauri shell, the React renderer, and a pass over the design documents plus the sandbox wrappers, path confinement, and policy engine. Each pass reproduced its important findings by executing code in scratch copies. The top finding of each pass was then re-checked against the source, and three were re-executed independently.

Evidence labels: **R** reproduced by execution, **S** confirmed by reading the source, **I** inferred (not executed, usually because it needs macOS, SSH, or a live provider).

## Verdict

The architecture holds up: the kernel protocol, the local-canonical mirror, the proposal model, and the typed IPC boundary are sound, and most findings are local fixes. But the control story the product is built on (review-gated edits, protected paths, sandboxed runs, data that stays put) currently has several ways around it, and a handful of defects will stop real use quickly. The first two tiers below should be fixed before the app is used on real work or pointed at a remote host.

## Tier 1: breaks a core guarantee

| ID | Finding | Evidence | Where |
|---|---|---|---|
| G1 | **A `Math:` docstring executes code in the kernel process, outside the sandbox.** The declared expression goes to SymPy's `parse_expr`, which is `eval`-based. The character filter allows `eval`, `chr`, parentheses, `+`, so any string can be built and evaluated. Opening a file whose equations are extracted is enough. The returned equation looks benign. | R (twice, independently) | `python/kernel/analyst_kernel/equations.py:239-253`, filter at `:56` |
| G2 | **Edit gating depends entirely on the OS sandbox, and the policy engine does not know which sandbox is active.** With the `limits` fallback, `exec.snippet` (policy class "run", auto by default, also available in Run mode) rewrote a protected source file and wrote to the home directory with no proposal. Network denial is likewise unenforced under `limits`. | R | `crates/core/src/policy.rs` (no sandbox input), `tools/registry.rs:68` |
| G3 | **`limits` is likely the default on Ubuntu.** Ubuntu 23.10 and later restrict unprivileged user namespaces through AppArmor, so the `bwrap` probe fails and the kernel silently falls back. Combined with G2, the default remote target runs agent code unsandboxed with auto-run. | I (platform knowledge; not tested on Ubuntu 24.04) | `sandbox.py:143-177` |
| G4 | **Edit tools can write `.git/config`, `.git/hooks/*`, and `.analyst/project.toml`.** Under `edits = "auto"` the agent declared scope `.git/config`, added a `diff.external` command, and the read-class `git.diff` executed it on the host. Writing `project.toml` escalates policy at the next open. The renderer's `fs_write`, `fs_rename`, and `fs_delete` also accept `.git`. | R | `crates/core/src/util.rs:33-55`, `tools/proposals.rs:34`, `tools/dispatch.rs:213-222`, `service/fs.rs:167-268` |
| G5 | **Scope declaration is self-granted.** `declare_scope` can be called repeatedly, replaces the scope silently, needs no approval, and is never reset between turns. The "out-of-scope edits still pause" guarantee is void. | R | `agent/mod.rs:509-530`, `policy.rs:77-84` |
| G6 | **Protected regions can be rewritten with a two-edit proposal.** The lock check tests each `old` string against the original file, but edits apply sequentially, so a second edit whose `old` only exists after the first is never checked. | R | `policy.rs:284-310` with `tools/proposals.rs:95-123` |
| G7 | **Symlinks escape the repo for read, list, and write**, in both the agent tools and the renderer's file commands. Path confinement is lexical only. Chat file attachments are not normalised at all, so an absolute or `..` path is read and sent to the model provider. | R (symlinks), S (attachments) | `tools/dispatch.rs:301-359`, `tools/proposals.rs:128,147`, `service/fs.rs:167-231`, `service/agent_runner.rs:302,320` |
| G8 | **Remote runs copy the entire run output directory back**, not only rendered artifacts, and do it twice for agent runs. This breaks the rule that data never leaves the host. | S | `target.rs:521-535`, `service/runtime.rs:845-872`, `tools/dispatch.rs:632` |
| G9 | **`out_dir` and `data_dirs` from the repo's `project.toml` are not confined.** `out_dir = "../x"` made the kernel create and write outside the repo, and that directory is what the sandbox makes writable. `data_dirs = ["~/.ssh"]` is expanded and handed to the sandbox as readable. | R (`out_dir`), I (`data_dirs`) | `runs.py:151,231-233,420-422`, `config.py:116` |

## Tier 2: will stop real use

| ID | Finding | Evidence | Where |
|---|---|---|---|
| B1 | **Runs hang forever on repos with roughly 2,800 or more files.** The sync file list writes every path to `git hash-object` before reading any output, so both pipes fill. Affects local targets too, and each attempt pins a runtime worker. | R | `target.rs:232-249` |
| B2 | **Reopening an existing session fails.** The renderer subscribes before opening; the backend rejects subscriptions for sessions that are not open. First click shows an error; second click opens with no event channel, so the kernel dot stays "starting" and nothing streams. The mock is forgiving and the tests only create sessions. | R | `apps/desktop/src/state/actions.ts:33`, `service/mod.rs:149-155` |
| B3 | **Context compaction can orphan a tool result.** With no plain user message to split at, the split falls to the last message, which can be a lone tool result. Both providers reject that request, and the orphan stays in history, so the conversation is stuck. Easy to hit at the 32k default window. | R (history shape), I (provider rejection) | `agent/mod.rs:771-774` |
| B4 | **Compaction does nothing once the current turn alone exceeds the budget**, and there is no fallback when the summary call fails. Ten summary calls in sixteen steps, then a context-length rejection. | R | `agent/mod.rs:757-778,813-817` |
| B5 | **Capability probing poisons its cache and sends parameters current models reject.** Any probe failure is stored as "text-protocol" forever; config overrides are ignored once an entry exists; the probe and compaction calls send `temperature: 0.0`, which current Claude models and OpenAI reasoning models reject. | R (cache), I (rejection) | `providers/capabilities.rs:64-99,146-150`, `agent/mod.rs:811` |
| B6 | **`max_tokens` is fixed at 4096.** A file-creation tool call over that size is truncated, executed as garbage, and retried until `max_steps`. A stream that ends without a terminal event is reported as a normal end. | R (parsing) | `agent/mod.rs:58`, `providers/anthropic.rs:187-203`, `providers/openai_compat.rs:110-112` |
| B7 | **Run output freezes the UI.** Output is one growing string re-rendered into a `<pre>` on every chunk: 0.6 MB costs 7.6 s of main-thread time, 2.5 MB costs 149 s. The kernel side has the mirror problem: 300 MB of output becomes a single 300 MB notification and 1.2 GB of kernel memory. | R (both) | `apps/desktop/src/ui/bottom/RunOutputPanel.tsx:80`, `runs.py:606-626,465-475` |
| B8 | **Undo after opening a file empties it**, and a save then writes an empty file with no conflict. The initial load is an ordinary undoable transaction. | R | `apps/desktop/src/ui/editor/EditorPanel.tsx:64` |
| B9 | **The run form builds wrong arguments** for `store_false` flags (unreachable), positionals (the name is sent as an argument), list defaults, and other actions. It also keeps stale defaults after the script changes and silently overrides the new default. Wrong arguments silently run the wrong analysis. | R | `apps/desktop/src/ui/sidebar/ScriptsView.tsx:27-41,96` |
| B10 | **The 8 GiB address-space cap breaks large memory-mapped data.** `np.memmap` of a 10 GiB file fails with ENOMEM. The cap is not configurable, and applies under every sandbox kind on Linux. | R | `sandbox.py:17`, `runs.py:282-285` |
| B11 | **Checkpoint refs collide.** Ids are `<session>-<per-agent counter>` and the counter restarts for each conversation and each CLI process, so later turns overwrite earlier undo points. | R | `agent/mod.rs:277-282` |
| B12 | **A packaged app cannot start a kernel.** There is no local bootstrap of the tooling environment and no bundled `uv` or `rg`; resolution falls back to `python3` on `PATH`. Apps launched from Finder or a desktop launcher also do not inherit the shell `PATH` or the API key environment variables. Works only from a dev checkout started in a terminal. | S | `kernel/transport.rs:183-221`, `tauri.conf.json` (no `externalBin`), `config.rs:147-149` |
| B13 | **On Linux and Windows, terminal control keys are captured by the app.** Ctrl+R, W, L, P, B, J, G, S never reach the shell. On macOS, Cmd+W likely closes the only window, and nothing guards unsaved buffers on any close path. | R (Linux), I (macOS) | `apps/desktop/src/ui/useAppCommands.ts:12-17,103-118`, `src/keymap.ts:14-25` |

## Tier 3: remote targets

None of these could be executed here (no SSH), except where marked.

| ID | Finding | Evidence | Where |
|---|---|---|---|
| R1 | **The mirror never loses deleted files.** `rsync --delete --files-from` deletes nothing; the code and PROTOCOL section 7 assume it does. Remote runs can import deleted modules and the index lists ghost scripts. | R (local rsync, same flags) | `target.rs:298-311` |
| R2 | **Two repos with the same directory name share one mirror**, and sync is skipped on a local-only hash, so one session can run the other's code. | I | `target.rs:77-86`, `runtime.rs:880-893` |
| R3 | **A fresh remote session cannot start.** Bootstrap and the session venv run only when the target is changed, not on create or open; the kernel exits because its root does not exist, and `run_start` starts the kernel before syncing. | I | `runtime.rs:1280-1302`, `service/mod.rs:203-249` |
| R4 | **Index, diagnostics, and equations come from a stale mirror** until the next run, because only runs and package tasks sync. | I | `runtime.rs:1043,1103,1150,1161` |
| R5 | **Remote package changes are lost.** `uv add` edits only the mirror's `pyproject.toml` and lockfile; the next push overwrites them. The mirror path is also mis-quoted when it contains spaces. | I (logic), R (quoting) | `runtime.rs:1229-1243`, `tools/dispatch.rs:727-737` |
| R6 | **No SSH timeouts or keepalives, and no per-call kernel timeouts.** After laptop sleep, pending requests never fail and the state stays "ready". Hosts starting with `-` are parsed as options; rsync and scp remote paths are unquoted. | I | `kernel/transport.rs:139-153`, `kernel/client.rs:200-220` |
| R7 | **Changing target leaves the UI showing a kernel error.** The old event forwarder is not stopped and reports "connection lost" for the replaced client. | R (with the fake kernel) | `runtime.rs:621-626,674-712` |
| R8 | **Timed-out connects leak an SSH process each**, because the reader task keeps the child alive. | R | `kernel/client.rs:103`, `transport.rs:72` |

## Tier 4: correctness and robustness

**Kernel**

| ID | Finding | Evidence | Where |
|---|---|---|---|
| K1 | Constant powers are evaluated eagerly with no budget (`9 ** 9 ** 9` froze the whole kernel), and the simplify "budget" abandons a thread that keeps burning CPU. | R | `equations.py:180-181,211-227` |
| K2 | Artifacts are recorded at their first stable size and never re-examined; a file opened early and written late is recorded as 0 bytes. | R | `runs.py:518-527` |
| K3 | The figure sidecar overwrites a user data file with the same stem (`results.json` next to `results.png`). | R | `analyst_mpl/__init__.py:147-154` |
| K4 | Run liveness is a bare pid. Runs whose supervisor died stay "running" after a restart, and cancel signals whatever process group now owns that pid. | R (reuse emulated) | `runs.py:337-347` |
| K5 | The REPL protocol shares fd 1 with the snippet. Any subprocess or C-level stdout write kills the REPL and silently wipes its state. | R | `snippet.py:25-26,65,161-165` |
| K6 | `eq.check` uses a fixed 1e-9 absolute floor, so any quantity below that scale "agrees" with anything (Planck-scale example reported agree with relative error 95). | R | `equations.py:545` |
| K7 | A malformed `Math:` section is dropped when derivation succeeds, and the check then compares the code with itself and reports agreement. | R | `equations.py:456` |
| K8 | `plt.pause()` and `show(block=False)` destroy the figure, so live-update loops save only the first frame. | R | `analyst_mpl/__init__.py:178-184` |
| K9 | Shutdown and unrelated calls block behind a running snippet; nine queued snippets starve the request pool. | R | `kernel.py:160-164`, `snippet.py:131` |
| K10 | On timeout only the direct child is killed; a worker that ignores SIGTERM survives. Attach after restart can deliver output twice. Invalid UTF-8 at end of output is never flushed. | R | `_supervise.py:111-128`, `runs.py:355-361,613-622` |

**Contract checker** (these will block the agent on ordinary code)

| ID | Finding | Evidence | Where |
|---|---|---|---|
| C1 | `cli.no-absolute-paths` flags f-strings such as `f"{out_dir}/spectrum.png"`. | R | `contract.py:208-215` |
| C2 | `lib.duplicate` flags the `build_parser()` functions the contract itself requires, since names and constants are normalised away. | R | `contract.py:317-324` |
| C3 | `cli.option-for-every-input` treats axis labels and log messages containing `/` as paths and `alpha=0.4`, `dpi=150` as inputs, while `open("data/x.csv")` is exempt as a builtin. Six errors on one ordinary plotting function. | R | `contract.py:366-369` |
| C4 | Checking specific paths compares only those files with each other, so duplicates of library functions are missed in exactly the case the agent uses. | R | `contract.py:132` |
| C5 | Near-duplicate search is all-pairs: 28 s for 240 similar functions, on every check. | R | `similarity.py:213-226` |

**Agent core**

| ID | Finding | Evidence | Where |
|---|---|---|---|
| A1 | Early exits (max steps, model error, cancel) leave consecutive same-role messages, skip the turn summary, and then tell the model the user changed files the agent itself wrote. | R | `agent/mod.rs:318-344,423-429,488` |
| A2 | No connect or idle timeout and no retry for 429 or overload; `stream_options` rejection is not tolerated although the protocol requires it; one invalid UTF-8 byte stalls the stream parser. | I, R (stall) | `providers/*.rs`, `sse.rs:28-33` |
| A3 | Tool calls are keyed only by index. Servers that reuse index 0 produce merged calls; missing ids repeat as `call_0` and collide in the transcript. | R | `providers/openai_compat.rs:195-241` |
| A4 | Whitespace-only assistant text is stored and sent; the Anthropic API rejects it and the conversation is stuck. | R (body), I (rejection) | `agent/mod.rs:377-381`, `providers/anthropic.rs:48` |
| A5 | `fs.search` ignores ripgrep's exit status, so an invalid regex returns "no matches" and the agent concludes the function does not exist. | R | `tools/dispatch.rs:405-435` |
| A6 | Spilled tool results cannot be paged: a long string is one line, so reading the artifact truncates at the same place. | R | `tools/dispatch.rs:256-266,315-336` |
| A7 | JSONL appends are two writes and two writers race on `runs.jsonl`; one torn line makes the whole file unreadable, permanently. | R | `session.rs:212-244` |
| A8 | Checkpoints re-hash the tree up to three times per turn and copy every untracked file into `.git/objects`; the template does not ignore `data/`. One 200 MB file adds 200 MB to `.git`. | R | `checkpoint.rs:48-78` |
| A9 | Package names starting with `-` are passed to `uv` as options. CRLF files can never be edited by `old`/`new`. A file the user creates while a create-proposal is pending is overwritten. | R | `tools/dispatch.rs:722-731`, `tools/proposals.rs:95-151` |
| A10 | Cancelling a turn does not cancel the script it started; a panic in a turn leaves the conversation busy forever; pending proposals are not recoverable after a webview reload. | R (cancel), I | `service/agent_runner.rs:400-419,596-618` |
| A11 | Reopened conversations start with empty model history; scope and pinned facts are not persisted. | S | `service/agent_runner.rs` |

**Service and renderer**

| ID | Finding | Evidence | Where |
|---|---|---|---|
| U1 | Terminal output shares one lossy broadcast with control events. A flood drops `proposal`, `turn_done`, or `run_done`, and nothing resyncs. | R (core level) | `service/terminal.rs:298`, `runtime.rs:688` |
| U2 | The file watcher covers `.venv`, `.git`, and all of `out/`; on hosts with the default inotify limit it fails silently and the session gets no file events. | R (watch count), I (failure) | `runtime.rs:1677` |
| U3 | `figures_list` re-parses the run history per run: 5.7 s for 500 runs, on a runtime worker. | R | `runtime.rs:1345-1398`, `service/figures.rs:164-198` |
| U4 | Parallel `session_open` calls build two runtimes and leak one. | R | `service/mod.rs:252-268` |
| U5 | Transcripts are never loaded, so existing conversations render empty after a restart. | S | `apps/desktop/src/ui/chat/ChatPanel.tsx:27-29` |
| U6 | CRLF files open dirty and are rewritten with LF on save. Text typed during an in-flight save is marked saved. | R | `EditorPanel.tsx:41`, `state/files.ts:101,111` |
| U7 | Expanded folders lose their children after any file change. Only the active terminal's screen exists, so switching tabs loses scrollback. | R | `state/files.ts:75`, `bottom/TerminalPanel.tsx:44` |
| U8 | Session load waits on the kernel: runs and figures stay empty until it is up, and the create dialog reports failure for a session that was created. | I | `state/actions.ts:39-65` |
| U9 | In dev mode (React StrictMode) the dock controller is cleared and no file can be opened. | R | `ui/workbench/Workbench.tsx:126-131` |
| U10 | The production CSP allows scripts from `localhost:5173`, and the capability grants window creation to all windows though the app creates none. No HTML injection path was found, so this is hardening. | S | `tauri.conf.json:26`, `capabilities/default.json` |
| U11 | The local terminal shell inherits provider API keys from the app environment. | S | `service/terminal.rs` |

## Design documents versus implementation

| Document says | Implementation |
|---|---|
| The sandbox hides everything except the repo, venv, interpreter, and data directories (ARCHITECTURE 5.6, 8) | Everything outside the home directory is readable (`--ro-bind / /`, `allow file-read* "/"`). On macOS all of `/private/tmp` and `/private/var/folders` is writable, so a repo under `/tmp` is not read-only. |
| Network grants take a host allowlist | Boolean only. An allowlist needs a proxy; the document should say so or drop it. |
| Memory cap and CPU count are configurable | Fixed 8 GiB address space, no CPU limit, not configurable (see B10). |
| API keys live in the OS keychain | Environment variables only, with no way to enter a key in the app (see B12). |
| Language server on the target; hover and definitions in the editor | Not built. VISION lists it under "first version". |
| `fig.get` returns images to vision-capable models; "send to agent" attaches the image | Not in the tool set; the attachment path is unverified. |
| SQLite indexes | Files only. PROTOCOL notes this; ARCHITECTURE D13 and section 7 do not. |
| Types generated with `specta` (section 11) | `ts-rs`. |
| Checkpoints pruned by age and count | Count only. |
| Scope declaration and protected paths apply in every edit mode | `auto-new-files` creates ignore scope; protected globs are case-sensitive, so they can be bypassed on a case-insensitive macOS volume (I). |

Two specification-level problems rather than code bugs: the `eq.check` tolerance rule (K6) and the wording of `cli.option-for-every-input` (C3) are implemented as written, and the writing is what needs to change.

## Tests that pass for the wrong reason

- The evaluation graders can pass with no agent work: the grader's own `run_succeeds` run satisfies `runs_at_least` and `artifact_exists`; `no_edits` inspects proposals, not the tree, so snippet writes are invisible; `edits_within_scope` ignores ordering; `unwanted_edits` counts in-scope accepts. Each task also writes into the real `~/.analyst`.
- The mock backend is more forgiving than the real one (B2), and no test opens an existing session.
- The desktop smoke script reads `window.__analystErrors`, which nothing defines, so "page errors: 0" is vacuous.
- The merge-result helper is tested but unused; the real accept path has no test.
- The macOS `sandbox-exec` profile has never been executed by any test, and its second fix is still unconfirmed on a Mac.

## Checked and found sound

Kernel path traversal checks for outputs and equations; argv-only subprocess calls throughout; the run environment allowlist (keys do not reach scripts); tier A expression transpiling (precedence, reassignment, unsupported operators rejected); the single-writer stdout protocol; SSE parsing across chunk boundaries; normal-turn message construction for both providers; API keys not reaching logs or transcripts; base-hash conflict detection for edits; checkpoints not touching the user's index; kernel request correlation and failure on kernel death; proposal double-respond handling; Tauri command and argument naming; and the renderer's handling of untrusted text (React text nodes, KaTeX without `trust`, figures through `<img>`, no inline script).

## Suggested order of work

1. **Close the guarantee gaps (G1 to G9).** Replace `parse_expr` with the existing whitelisting AST walker, run in a subprocess. Make the policy engine sandbox-aware: under `limits` or `none`, runs and snippets ask, and the UI says why. Hard-deny `.git/**` and `.analyst/**` for edits. Require approval to widen scope and reset it per turn. Check locks against the post-edit text. Canonicalise paths. Pull only rendered artifacts. Confine `out_dir` and `data_dirs`.
2. **Decide the Ubuntu sandbox story (G3).** Options: document an AppArmor profile for `bwrap`, add a Landlock backend that needs no user namespaces, or accept ask-by-default on such hosts.
3. **Unblock daily use (B1 to B13).** The sync deadlock, session reopen, compaction, probe cache, `max_tokens`, output handling, editor undo, run form, memory cap, checkpoint ids, packaging, and key handling.
4. **Remote targets (R1 to R8)** before the first run against a real host.
5. **Contract false positives (C1 to C4)**, since they will make the agent loop on correct code.
6. **Harden the tests** so the mock, the graders, and the smoke script cannot pass vacuously, and add a macOS sandbox test to CI.
7. **Bring the documents in line** with what is built, or mark the gaps as planned.
