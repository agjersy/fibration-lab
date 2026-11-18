# Personal Analyst — Application Design and Architecture

*Status: phases 0 and 1 built; revised after the November 2026 implementation review ([REVIEW.md](REVIEW.md)). Companion documents: [VISION.md](VISION.md), [OPEN-QUESTIONS.md](OPEN-QUESTIONS.md).*

**One product (November 2026).** Personal Present has been folded in. There is one core (`crates/core` with the deck library `analyst-deck` in `present/crates/core`), one desktop shell, one `analyst` CLI, one agent loop, one policy engine, one transcript, and one undo timeline; there is no bus, no MCP, and no bridge between workbenches. A project is one git repository; its decks live under `decks/<slug>/` (section 4) and are checkpointed by git with the code (5.9). Slides cite analysis runs through the artifact index (5.12). The old deck home `~/.present` is migrated into `~/.analyst` once (5.16).

This document specifies the architecture of the desktop application described in the vision. It is written to be built from. Decisions confirmed in review are marked as such; where a decision is still open, it states the recommendation and points to the open-questions document.

## 1. Decision summary

| # | Decision | Resolution | Status |
|---|---|---|---|
| D1 | Desktop shell | Tauri 2: a host-native desktop application (native window, menus, dialogs, installers; no browser, no local server). Rust core; the interface is drawn by the OS's embedded webview component (WKWebView on macOS, WebKitGTK on Linux) | Decided |
| D2 | Analysis runtime | Python, one `uv`-managed virtual environment per session. `uv` only; no conda | Decided |
| D3 | App tooling runtime | A hidden, app-owned Python environment per execution target for the kernel, SymPy, and Ruff (language servers later) | Decided |
| D4 | Model access | One internal interface, two wire adapters (OpenAI-compatible chat completions, Anthropic Messages). No model-specific code paths; capabilities probed, not hand-listed | Decided |
| D5 | Execution targets | Default is fully local. The default remote target is a native environment on a remote Linux machine (Ubuntu or Fedora) over SSH; a cluster login node is the same target kind. Kernel protocol is transport-agnostic. Local repo is canonical; remote holds a synced mirror. Data is never synced; only rendered products (figures, logs, metadata, equation results) return to the UI | Decided |
| D6 | Sandbox | `bubblewrap` on Linux (local and remote), `sandbox-exec` on macOS, no network for sandboxed code, kernels, or terminals (no per-run grants), container backend optional, unsandboxed with a visible badge where neither exists | Decided |
| D7 | Edit gating default | Review mode with per-hunk accept and reject; protected paths and protected-region markers in the first version | Decided |
| D8 | Equation source | AST-derived for pure expressions, declared-and-checked for everything else, LLM-explained marked unverified | Open, see OQ-5 |
| D9 | Script contract | CLI options on every script and shared logic in the library package are hard rules; the remaining proposed rules ship as defaults with severities adjustable per repo | Decided |
| D10 | Editor | CodeMirror 6 (Monaco as the fallback if a needed feature is missing) | Recommended |
| D11 | Docking | `dockview` for in-window panels; Tauri `WebviewWindow` for undocked panels | Recommended |
| D12 | LaTeX toolchain | A TeX installation the user provides. Tectonic's fetch-on-demand is removed from the plan: the app downloads nothing for it | Decided, see OQ-10 |
| D13 | Persistence | JSONL for transcripts and run records, TOML for config, git refs for checkpoints. A SQLite index is deferred until file scans become a measured problem | Decided |
| D14 | External tools | Dropped for good: there is no MCP client, no MCP policy key, and no MCP tool naming | Dropped |
| D15 | Evaluation | Model-agnostic task suite with automatic graders; produces a scorecard per configured model; no model is privileged | Decided |
| D16 | Platforms | macOS and Linux first, for both the desktop app and remote targets. Windows deferred to phase 4 | Decided |
| D17 | Run launcher | Runs start directly on the target (local process, remote Linux machine, or cluster login node) behind a launcher interface; a Slurm scheduler launcher is a planned later extension, not a first-version feature | Decided |
| D18 | Name and license | "Personal Analyst": binary `analyst`, config `~/.analyst`, bundle id `dev.personal-analyst.app`. Private for now; permissively licensed dependencies only so the license choice stays open | Decided |

## 2. System overview

```mermaid
flowchart LR
  subgraph UI["Renderer (React, system webview)"]
    Chat[Agent chat]
    Editor[Editor + diff review]
    Figs[Figures panel]
    Eqs[Equations panel]
    Pkgs[Packages panel]
    Docs[Markdown / PDF preview]
    Term[Terminal]
  end

  subgraph Core["Core (Tauri backend, Rust)"]
    SM[Session manager]
    AL[Agent loop]
    PE[Policy engine + edit gating]
    MP[Model providers]
    TR[Tool registry]
    TG[Target manager\nlocal | ssh]
    EM[Environment manager]
    RK[Kernel client]
    SY[Repo sync]
    FS[File + git service]
    CK[Checkpoints]
    ST[(JSONL / TOML)]
  end

  subgraph Target["Execution target (local machine or SSH host)"]
    K[analyst-kernel\n(tooling env)]
    SB[Sandbox]
    V[(Session venv\nvia uv)]
    Script[User script\n+ figure capture backend]
    Mirror[(Repo mirror\nremote only)]
  end

  LLM[(Ollama / OpenAI-compatible\nAnthropic)]
  Git[(Local analysis repo)]

  UI <-- typed IPC --> Core
  AL --> MP --> LLM
  AL --> TR
  AL --> PE
  AL --> RK
  TG --> RK
  TG --> SY
  RK -- stdio or ssh --> K
  SY -- rsync / tar over ssh --> Mirror
  K --> SB --> Script
  Script --> V
  EM --> V
  FS --> Git
  CK --> Git
```

Three tiers:

- **Renderer.** All visible surfaces. Holds no authority: it cannot write files, run code, or call models except through typed IPC requests to the core.
- **Core.** Owns sessions, the agent loop, policy, model access, tools, targets, and persistence. One process per app, one session context per tab. The analysis repo on the local disk is always the source of truth for code.
- **Execution target.** Where the kernel, the sandbox, the session venv, and the user's scripts run. It is either the local machine or a remote host reached over SSH. The core talks to the kernel with the same protocol either way.

## 3. Desktop stack (D1)

**Decided: Tauri 2, built as a host-native desktop application for macOS and Linux.**

What "native" means here: the app is a single native executable with native windows, menus, file dialogs, keyboard handling, multi-window support, and platform installers (`.app` and `.dmg`, `.deb` and `.rpm`). There is no browser process, no local HTTP server, and no dependency on a browser being installed. The interface itself is drawn by the operating system's embedded webview component (WKWebView on macOS, WebKitGTK on Linux), the same component system applications use to render rich content; all interface assets are bundled inside the executable and loaded from it. The Rust core owns every capability that touches the machine (files, processes, SSH, model calls); the interface layer only draws and forwards intent.

| Layer | Choice | Notes |
|---|---|---|
| Shell | Tauri 2 | Native window, menus, dialogs, multi-window for undocked panels, sidecar binaries, platform installers (updates ship as new installers; there is no auto-updater). Interface drawn by the OS webview component: WKWebView on macOS, WebKitGTK on Linux. |
| Core | Rust on `tokio` | All services in section 5. Compiled into the Tauri backend; also linked into a headless CLI for phase 0 and evals. |
| IPC | Tauri commands and per-session channels; types exported with `ts-rs` | Request/response as commands; streams (model tokens, run output, artifacts) as events on a per-session channel. The `protocol` crate's types produce the TypeScript bindings. The contract is [IPC.md](IPC.md). |
| Model clients | SSE streaming over requests built by the network gate (`wb-net`), hand-written adapters | Two adapters are a few hundred lines each and keep full control over tool-call parsing. No provider SDKs. |
| Storage | `toml`, JSONL | See section 7. |
| File watching | `notify` | Working-tree diffing between turns, `out/` watching for terminal runs. |
| Terminals | `portable-pty` in the core, `xterm.js` in the renderer | A remote terminal is a local pty running `ssh -t`. |
| Git | System `git` through the CLI | Plumbing commands for checkpoints; porcelain for status and diff. |
| SSH | System OpenSSH client (`ssh`, `scp`), `rsync` where present | See section 5.5. No SSH reimplementation in the first version. |
| Editor | CodeMirror 6 | Lighter and more consistent across the three webviews than Monaco. Python syntax, inline diff widgets, lint gutter. An LSP client is planned, not built. |
| Language tooling (planned) | Today: Ruff diagnostics through the kernel. Planned: `ruff server` and `ty` or `jedi-language-server` run from the tooling environment on the target | No Node dependency. The core proxies LSP over IPC, so on a remote target the language server runs where the venv is and sees the real packages. |
| Docking | `dockview` | Floating groups in-window; undock creates a second `WebviewWindow` bound to the same session. |
| Rendering | KaTeX, `react-markdown` with `remark-math`, `pdf.js` | |
| Sidecars | `uv`, `rg` | Bundled per platform through Tauri's external-binary mechanism. |

Known cost: WebKitGTK on Linux is slower than Chromium for heavy canvases and large DOMs. The figures panel and editor must be tested on Linux early, and the figure viewer uses `<img>` elements with cached raster files rather than live canvases.

## 4. Domain model

| Entity | Definition | Owned by |
|---|---|---|
| Workspace | The app-level state: provider config, targets, global skills, list of sessions. | `~/.analyst/` |
| Target | Where runs execute: `local`, or an SSH host with a remote root directory. Holds its own tooling env, venvs, and run records. | `~/.analyst/config.toml` |
| Session | Exactly one repo + one target + one environment + one or more conversations + layout + run history. One tab per session. A library shared across repos is a package dependency of each repo, not a second repo in the session. | `~/.analyst/sessions/<id>/` |
| Repo | A git repository on the local disk containing analysis code and the committed project config in `.analyst/`. Canonical for code. | user's filesystem |
| Mirror | The synced copy of the repo on a remote target. Never edited directly; rebuilt from the local repo before runs. | `<remote_root>/<repo>` on the target |
| Environment | A Python virtual environment managed by `uv`, living on the target. | `<repo>/.venv` or `<mirror>/.venv` |
| Conversation | An ordered list of turns with one model configuration. A session can have several. | session store (JSONL) |
| Turn | One user message and everything the agent did in response: model calls, tool calls, proposals, runs. | transcript |
| Proposal | A pending change set (file edits, package installs, commands) awaiting policy decision. | core, in memory + transcript |
| Run | One execution of a script with arguments on a target, producing stdout, stderr, exit code, artifacts, and timing. Survives disconnects. | `runs.jsonl` in the session store; `~/.analyst/runs/<id>/` on the target |
| Artifact | A file produced by a run, with a sidecar metadata record. Rendered artifacts (figures, logs, run metadata) are pulled back to the local `out/` from remote targets; data artifacts (tables, arrays, derived datasets) stay on the target and are read on demand. | `<repo>/out/` locally; `<mirror>/out/` on the target |
| Equation | A rendered mathematical statement bound to a code location, with a provenance tier and verification status. | computed on demand |
| Checkpoint | A snapshot of the local working tree taken before an agent turn that may change files. | git ref `refs/analyst/checkpoints/<id>` |
| Skill | A markdown instruction set with trigger metadata, loaded into the agent's context on demand. | `.analyst/skills/`, `~/.analyst/skills/` |
| Provider / Model | A configured endpoint plus a model identifier and its probed capability record. | `~/.analyst/config.toml`; keys from the environment or `.env` |
| Policy | The per-session permission rules the agent operates under. | `.analyst/project.toml` with session overrides |
| Deck | A presentation in the repo: `decks/<slug>/` with `deck/` (deck and slide documents, or a build script in code mode), `template/`, `references/`, `assets/`, `exports/` (ignored), and its own config `.present/project.toml` (`.present/cache/` ignored). A session lists and creates decks; a deck's kernel (no network) starts on first use; the reference index is per-user state under `~/.analyst/sessions/<id>/decks/<slug>/`. Decks are excluded from remote sync and never go to remote targets. | the repo; session store for the index |

## 5. Core services

### 5.1 Session manager

Responsibilities: create, open, close, and switch sessions; restore layout; keep each session's target connection, kernel, terminals, file watcher, and conversation alive while open; suspend them when closed.

Creating a session:

1. Choose a repo (existing directory, `git init` a new one, or clone). Optionally apply a template that creates `scripts/`, `analysis/`, `data/`, `out/`, `notes.md`, and `.analyst/project.toml`.
2. Choose a target: local or one of the configured SSH hosts. For a remote target the core bootstraps the host if needed (5.5) and performs the first sync.
3. Bind an environment on the target: adopt an existing `.venv`, or run `uv venv` plus `uv sync` from the repo's `pyproject.toml` or `requirements.txt`.
4. Start the kernel on the target, index scripts and the library, and compute the project map.
5. Choose a model (defaults from workspace config).

A session can change target later; doing so re-binds the environment and re-syncs. Session tabs persist across app restarts. Closing a tab suspends the session; it does not delete anything.

### 5.2 Environment manager (D2)

`uv` is the only environment tool. It is fast and produces a lockfile. It uses a Python interpreter that is already installed on the machine and never downloads one: every `uv` the core starts, locally and on remote hosts, runs with `UV_PYTHON_DOWNLOADS=never`, and when no suitable interpreter exists the operation fails with a message that names the required version and says to install it on that machine. The app bundles the `uv` binary for the local target and copies a `uv` binary to remote targets that have none (5.5). Conda is out of scope; packages must be installable from the one configured index: `[packages] index_url` in `config.toml`, public PyPI by default or an internal mirror. Every `uv` invocation is made by the core. Installs (`uv add`, `uv remove`, `uv sync`, `uv lock`, venv creation, the tooling bootstrap) are authorized by the network gate (5.6) and pass the index to `uv` explicitly (`--default-index <url>`); every other `uv` call runs with `UV_OFFLINE=1`.

**Projects may not name their own package sources.** Before an authorized install the core reads the project's `pyproject.toml` (and those of its workspace members and in-repo path sources), `uv.toml`, `uv.lock`, and `requirements.txt`, and refuses, naming the entry and the file, when they contain: `[[tool.uv.index]]` entries or `index-url` / `extra-index-url` / `find-links` settings that point anywhere other than the configured index; `tool.uv.sources` entries with `git` or `url` (path sources inside the repo are allowed); dependencies written as direct URL or VCS references (`name @ https://...`, `git+...`); and lockfile packages whose source is `git`, `url`, or a registry other than the configured index. The check is in the core (`egress::sources`), runs for local installs when the command is spawned, and runs for remote installs against the local project, which is what the mirror holds.

Operations exposed to both the user (Packages panel) and the agent (tools, gated by policy), all executed on the session's target:

- list installed packages with versions and whether they are in the lockfile
- add or remove a package (`uv add` / `uv remove` when `pyproject.toml` exists, else `uv pip install`)
- sync to lockfile
- choose Python version
- show outdated packages

Because the lockfile lives in the local repo and is synced, the environment is reproducible on any target: switching a session from local to a remote host is `uv sync` on the other side.

Every operation is logged to the session and is a visible card in the conversation if the agent initiated it. Package changes by the agent default to "ask".

### 5.3 App tooling environment (D3)

Each target carries an app-owned Python environment at `~/.analyst/tooling/` (created with `uv python install` at a pinned version) containing `analyst-kernel`, SymPy, Ruff, and the language server. The kernel wheel ships inside the app bundle and is copied to the target, so bootstrapping a remote host needs PyPI only for SymPy and friends. Session environments are never polluted with app dependencies. The only thing injected into a user script's process is a single stdlib-only module, `analyst_mpl`, placed on `PYTHONPATH` for figure capture.

### 5.4 Kernel protocol

The kernel is a long-lived process per session, launched from the tooling environment on the target with the session's interpreter path and repo (or mirror) root as arguments. Transport is JSON-RPC 2.0 over stdio. The core sees it through a `KernelTransport` trait with two implementations, `LocalStdio` and `SshStdio`; nothing above the transport knows which one is in use.

| Method | Purpose |
|---|---|
| `env.info` | Interpreter path, version, site-packages, sandbox availability, host name. |
| `env.packages` | Installed packages (delegates to `uv`). |
| `scripts.index` | Walk `scripts/` (configurable globs) and parse each file's `argparse` definitions with the AST, without importing. Returns name, description, options, defaults, types. |
| `lib.index` | Walk the library package (`analysis/` by default) and return every public function with signature, docstring summary, `Math:` presence, and the scripts that call it. Used by the agent before writing new functions and by the generalization checks. |
| `run.start` | Execute a script with arguments inside the sandbox using the session interpreter. The run is detached (`setsid`) with output and status written to `~/.analyst/runs/<run-id>/` on the target. Returns a run id and streams `run.stdout`, `run.stderr`, `run.artifact`, and `run.done` notifications. |
| `run.attach` | Resume streaming a run from recorded offsets after a kernel restart or a dropped connection. |
| `run.cancel` | Terminate a run (process group). |
| `out.list` | List a run's output directory on the target with sizes and types, without transferring anything. |
| `out.read` | Return a bounded preview of a non-rendered artifact on the target (head of a CSV, shape and summary of an array, first rows of a table). The agent and the UI inspect data outputs through this call; the files themselves are not transferred. |
| `exec.snippet` | Run a short snippet in a persistent REPL subprocess in the session venv, also sandboxed. For quick checks by the agent or the user. |
| `eq.extract` | Equation extraction for a file or function (section 5.11). |
| `eq.check` | Numeric consistency check of a declared equation against a function. |
| `contract.check` | Script contract checker (section 5.13). |
| `contract.similar` | Near-duplicate function detection across scripts and the library (section 5.13). |
| `lint.run` | Ruff diagnostics for a file. |

The REPL subprocess for `exec.snippet` is the only stateful execution surface; script runs are always fresh processes so results are reproducible from the terminal. The kernel holds no state that is not on disk in the run directory, so a kernel restart is always safe.

### 5.5 Execution targets and remote execution over SSH (D5)

**Decided: remote execution is a first-version feature, designed in from phase 0.**

A target is where code runs. The session's editor, agent, policy engine, checkpoints, and transcripts are always local; the kernel, sandbox, venv, data, and runs are on the target.

Target kinds, in order of default:

| Kind | Description | Status |
|---|---|---|
| `local` | Everything on this machine. The default for every new session; nothing needs configuring. | First version |
| `ssh` | A native environment on a remote Linux machine (Ubuntu or Fedora are the supported distributions) reached over SSH: `uv`, the tooling environment, and the session venv are installed in the user's home directory on that machine. A cluster login node is this same kind; the app does not need to know it is a cluster. | First version |
| `ssh` + scheduler launcher | The same target with runs submitted through Slurm instead of started directly. | Planned extension, not first version |

**Configuration**

```toml
[[targets]]
name = "lab-gpu"
kind = "ssh"
host = "lab-gpu"                # an alias from ~/.ssh/config; keys, jump hosts, and agent forwarding come from there
remote_root = "~/analyst"       # mirrors live at <remote_root>/<repo-name>
python = "3.12"                 # must be installed on the host; never downloaded
sandbox = "auto"                # auto | bwrap | none
data_dirs = ["/data/shared/run3"]   # read-only mounts for the sandbox, target-local paths
```

The local target needs no configuration.

**Connection.** The core uses the system OpenSSH client with connection multiplexing (`ControlMaster=auto`, `ControlPersist`, a control socket under `~/.analyst/ssh/`). One master connection per target per session; the kernel's stdio, each terminal, and each sync are channels on it. This respects the user's existing SSH configuration, keys, agent, and bastions without the app reimplementing any of it. A Rust SSH implementation can replace this later behind the same `Transport` trait if the CLI dependency becomes a problem (Windows lacks `ControlMaster`; there the core opens separate connections).

**Bootstrap.** On first use of a target the core checks for `~/.analyst/bin/uv` and `~/.analyst/tooling/` on the host and creates them if missing. `uv` is never downloaded: a `uv` already on the host is used; otherwise a binary is copied over SSH (`scp`) to `~/.analyst/bin/uv`, the local `uv` when the host reports the same OS and architecture (`uname -sm`), else the file named by the target's `uv_binary` setting; with neither, bootstrap stops and says to provide one. Python is not downloaded either: the host must have the target's `python` version installed (`uv python find <version>` decides), and when it does not, bootstrap stops with a message naming that version and the host. Then the tooling environment from the kernel wheel in the app bundle. Nothing is installed outside the user's home directory and nothing needs root. Ubuntu and Fedora are the tested distributions; any Linux with a working `ssh`, `git`, `tar`, an installed Python of the configured version, and a `glibc` recent enough for `uv` should work. Bootstrap is idempotent and version-checked, so an app upgrade refreshes the remote kernel on next connect.

**Repo sync model: local canonical, remote mirror.** The local repo is the source of truth for code. Before each run, and on demand, the core syncs the mirror:

1. Write the working tree to a git tree object with the checkpoint machinery (5.9): tracked and untracked-but-not-ignored files, without data directories and files over the checkpoint size limit, through a temporary index so the user's index, branches, and refs are untouched. The repo's `.env` and the directories the host owns (`out_dir`, `.venv/`, `.analyst/`) are removed from that tree even when they are tracked. The tree id is the sync identity.
2. Ask the host what its mirror holds. The host keeps a bare repository beside the mirror, at `<mirror>.git`, created without hooks. When the mirror is already a checkout of this tree, nothing is sent.
3. Otherwise wrap the tree in a parentless commit (no history travels) and `git push` it over the multiplexed SSH connection to `refs/analyst/sync` in that repository. The push is authorized by the network gate like every SSH connection, and runs from a scratch repository that borrows the project's objects, with hooks off on both sides and without the user's, the system's, or the project's git configuration, so no hook, credential helper, or URL rewrite takes part.
4. Make the mirror a detached checkout of that commit (`git checkout -f --detach` with the mirror as work tree). Files that left the tree are deleted and changed files rewritten; `out_dir`, `.venv/`, `.analyst/`, and anything else the tree never held are untouched.

The same tree id is recorded with each run, so a run is attributed to an exact code state. The host needs `git`; without it the sync fails and says so.

Syncs are typically well under a second on a warm connection. The mirror is never edited in place; any file the agent or user changes is changed locally and re-synced.

**Only rendered products come back.** A run writes to `out/` on the target. `run.artifact` events carry the target path and metadata. The core pulls back only what the UI renders: figure rasters and vectors with their sidecar metadata, the run's stdout and stderr (bounded), the run metadata JSON, and equation extraction and check results (which are computed on the target and transmitted as LaTeX plus a status, never as code execution on the local side). Everything else a run produces, such as tables, arrays, pickles, and derived datasets, stays on the target. The agent and the user inspect those through `out.list` and `out.read`, which return bounded previews without transferring files. The figures panel therefore does not care where a figure was produced, and no dataset is ever copied to the local machine by the app.

**Data stays where it is generated.** Input data directories are configured per target and are never synced in either direction. Scripts refer to data through CLI options whose defaults are repo-relative or come from `.analyst/project.toml` per-target overrides, so the same script runs locally against a sample and remotely against the full dataset without edits. A user who wants a data artifact locally fetches it explicitly from the Outputs view; the app records that it did so but never does it on its own.

**Run launcher.** The kernel starts runs through a `Launcher` interface with one implementation in the first version, `DirectLauncher`, which runs the script as a detached process on the target: on a cluster this means the login node, which is the accepted mode for now. The interface (`submit`, `status`, `cancel`, `output_paths`) is shaped so that a `SchedulerLauncher` for Slurm or similar can be added later without changing the kernel protocol or the agent loop; the only visible differences would be queue time in the run card and a per-target launcher setting. In the code today the extension point is `Target::launcher()` in `crates/core/src/target.rs`, which always returns `Launcher::Direct`; `Launcher::Scheduler` is an unimplemented placeholder.

**Runs survive disconnects.** The kernel detaches each run and records stdout, stderr, exit status, and artifact list under `~/.analyst/runs/<run-id>/` on the target. If the SSH connection drops, the run continues. On reconnect, the core restarts the kernel if needed and calls `run.attach` to resume streaming from the last offsets. The run card shows "running, disconnected" rather than failing.

**Terminal.** A remote terminal is a local pty running `ssh -t <host>` that changes to the mirror directory, activates the venv, and exports the figure-capture environment. Where `bubblewrap` works on the host the remote shell (and the remote kernel) run under `bwrap --unshare-net`; otherwise they run unwrapped and the UI says the network is not isolated. Figures produced from that terminal appear in the figures panel through the same `out/` watcher, running on the target through the kernel.

**Sandbox on remote hosts.** Linux hosts with `bubblewrap` and unprivileged user namespaces get the same sandbox as a local Linux machine. Many shared hosts disable user namespaces; there the kernel falls back to resource limits (`ulimit`, timeouts) and the run is badged "unsandboxed" in the UI. The SSH account boundary and the policy engine still apply. Scheduler integration (submitting runs through Slurm or similar) is not designed here; see OQ-17.

**What the user sees.** The session tab shows the target name and connection state. When a remote target is unreachable, editing, chat planning, equation viewing, and reading past runs continue to work; run and package tools report the target as offline, and the agent is told so in its context.

**Rejected alternative.** A "remote-primary" mode where the repo lives only on the host and the local side is a cache (editing over SFTP) was considered and rejected in favor of the simpler local-canonical mirror. The repo always lives locally and is mirrored to the host.

### 5.6 Sandbox (D6)

**Decided:** OS-level isolation as below, no network for anything that runs code, container backend as an opt-in, resource-limits-only fallback badged in the UI on hosts that cannot isolate.

**Network rule.** The app reaches the network for three things only, and only from the Rust core: (1) model endpoints (the `base_url` of configured providers), (2) Python package installation from the configured index, and (3) SSH execution targets from `config.toml`. All three go through one gate, the `wb-net` crate: it builds every HTTP request and refuses hosts that are not configured provider endpoints, and it authorizes each `uv` install and each `ssh`/`scp`/`rsync` spawn (the core passes every command line it runs through one check, `egress::guard`). The allowlist is filled from the user's configuration, never from model output. Every decision, allowed or refused, is appended to the ledger `<home>/egress.jsonl` (destination and purpose, never content); if the ledger cannot be written the request is not made. Code the agent runs, the Python kernels, and the embedded terminal have no network: the kernel and the terminal shell are started under `bwrap --unshare-net` (Linux) or `sandbox-exec` with the network denied (macOS), and per-run sandboxes are stricter still. There are no per-run network grants and no setting that enables one; a `network = ...` key left in an old `project.toml` is ignored. Where no isolation tool works, the kernel state and the terminal panel show "network not isolated".

Scripts run with least privilege:

| Resource | Default |
|---|---|
| Filesystem read | The home directory is hidden except for the repo or mirror, the session venv, the interpreter's base installation, the tooling environment, and user-approved data directories. Everything outside home (system libraries, other mounts) is readable. Data directories outside the repo come only from the user's global config, never from the repo's `project.toml`. |
| Filesystem write | The run's output directory, `data/derived/` if present, and a per-run scratch directory that is also `TMPDIR`. Source directories are read-only during runs so a script cannot modify code. On macOS the system temp locations are also writable. |
| Network | Denied, always. There is no grant: not per run, not by policy, not by a tool parameter. |
| Processes | Can spawn child processes within the same sandbox. Cannot escalate. |
| Resources | Wall-clock timeout (default 10 minutes) and open-file limit. An address-space limit is opt-in (`[limits] address_space_gb`) because it also limits memory-mapped data. No CPU limit. |
| Environment variables | Minimal allowlist. API keys are never passed into the sandbox. |

Implementation per platform:

- **Linux (local or remote):** `bubblewrap` (`bwrap`) with bind mounts matching the table above and `--unshare-net`. Fallback to Landlock plus `seccomp` if `bwrap` is unavailable; fallback to resource limits only, badged, if user namespaces are disabled.
- **macOS:** `sandbox-exec` with a generated profile. It is deprecated in documentation but functional and used by other agent tools; the alternative is a container backend.
- **Container backend (optional):** Podman or Docker with the venv and repo mounted. Slower start, strongest isolation.
- **Windows:** deferred with the platform (D16).

The sandbox protects the host from the agent's code. It does not protect the repo from the agent's edits; that is the policy engine's job (5.8). The two are linked: review-gated edits only mean something if code the agent runs cannot write source files, so when the active sandbox is `limits` or `none` the policy engine makes every run, snippet, and shell call ask first (`policy.unsandboxed_runs`, default `ask`). Recent Ubuntu releases restrict the user namespaces `bubblewrap` needs, so that is the expected mode on a stock Ubuntu host until an AppArmor profile for `bwrap` is installed or a Landlock backend is added.

### 5.7 Agent loop

The loop is the product's core and is designed around the analysis cycle: plan, propose, run, verify. It is deliberately a single-threaded, inspectable loop rather than a multi-agent system, and it contains no model-specific branches.

**Context assembly for each model call**

1. System prompt: role, the script contract (5.13), output conventions, and the active policy summary so the model knows what will require approval.
2. Project map: directory tree (depth-limited), the script index (name, description, options), the library index (public functions with one-line docstrings), environment summary (Python version, key packages), target name and state, git status, and the list of files changed by the user since the last turn.
3. Skills (5.16): the list of available skills by description, and in full those whose legacy triggers match the turn.
4. Conversation history, compacted when it exceeds a configurable fraction of the model's context window. Compaction summarizes older turns and keeps a pinned-facts block (decisions, file names, parameter values) that the agent maintains explicitly.
5. The current user message plus any attachments: file references, figure references, selected text from the editor.

Large outputs (run logs, file contents) are never inlined past a size limit; they are stored as artifacts and the model receives a truncated view plus an artifact id it can read with a tool.

**Modes** (per conversation, switchable mid-conversation)

| Mode | Tools available | Use |
|---|---|---|
| Plan | Read-only: files, search, script and library index, equations, run history. | Discuss and plan before touching anything. |
| Build | Everything, subject to policy. | Write and change tools. |
| Run | Read-only plus `run.script`, `exec.snippet`, and artifact tools. No edits, no installs. | Exploration with existing tools, parameter sweeps, interpretation. |
| Outline | Read-only plus `outline.propose` and `export.run`. | A deck's storyline; no slide changes. |
| Slides | Read-only plus every deck tool and `slide.place_figure`; no code edits, script runs, packages, shell, or git. | Create and change slides. |
| Polish | Read-only plus slide updates and moves, deck code edits and builds, skill scripts, exports; nothing new (no slides, citations, or elements). | Improve existing slides. |

Modes are `wb-policy` mode rules (`crates/core/src/tools/gate.rs`, `mode_rules`): they decide which tools the model is offered and are enforced again at call time. The loop registers the deck tools (`present/crates/core/src/tools`) next to the analysis tools whenever the repo has a deck; slide documents are repo-relative files to the policy, so one `declare_scope` (files and slides) and one checkpoint per turn cover code and slides, and slide proposals keep their before/after shape in the transcript and the approval request.

**Tool set (built-in)**

| Tool | Policy class |
|---|---|
| `fs.read`, `fs.list`, `fs.search` (ripgrep, local) | read |
| `scripts.index`, `lib.index`, `runs.list`, `runs.get` | read |
| `eq.extract`, `eq.check`, `contract.check`, `contract.similar`, `lint.run` | read |
| `fig.list` (`fig.get`, returning images to vision-capable models, is planned) | read |
| `target.info` (name, state, sandbox level, data dirs) | read |
| `out.list`, `out.read` (bounded previews of data outputs on the target) | read |
| `fs.propose_edit` (unified diff or full file) | edit |
| `fs.propose_create`, `fs.propose_delete` | edit |
| `run.script`, `exec.snippet` | run |
| `pkg.add`, `pkg.remove`, `pkg.sync` | package |
| `shell.run` (inside sandbox on the target, arbitrary command) | shell |
| `git.status`, `git.diff` | read |
| `git.commit` | git |
| `doc.render`, `doc.insert_figure` | edit |
| `skill.load` | read |

**Turn algorithm**

```
on user message:
  if mode allows edits: create checkpoint
  diff working tree against last-known state; add user-changed files to context
  loop (bounded by max_steps and token budget):
    stream model response
    for each tool call, in order:
      decision = policy.evaluate(tool, args)
      if decision == deny:  return error result to model
      if decision == ask:   pause, show card in chat, await user; on reject return error result
      if tool is edit:      stage into proposal set; if policy == review, pause here (same as ask)
      if tool is run:       sync mirror if target is remote and tree changed
      execute tool; append result to context
    after any accepted edit: run lint + contract check (including similarity); feed diagnostics back
      (at most N auto-fix iterations, then surface to user)
    if model stops without tool calls: break
  summarize turn: files changed, runs made, figures produced, open issues
  update pinned facts
```

Pausing on "ask" is a real pause: the model's partial turn is held, the user acts in the UI, and the loop resumes with the decision as a tool result. This mirrors the Zed experience you already use, with finer-grained control.

**Verification steps built into the loop**

- After edits: Ruff, `py_compile`, and the contract checker run automatically, including the generalization checks. Failures go back to the model before the user sees a "done".
- After runs: non-zero exit codes and tracebacks go back to the model with the run's stderr. The user sees the run card either way.
- On request or by policy: `eq.check` runs on functions with declared equations; mismatches are surfaced as a red badge in the Equations panel and as a tool result the model must address.

**Scope declaration.** In Build mode the agent's first action on a new task is a short plan listing the files it intends to touch. Edits outside the declared set require approval even when the session is in auto-accept mode. The scope resets every turn, and widening it within a turn itself requires approval, so the agent cannot grant itself more. Paths under `.git/` and `.analyst/` are never editable by the agent. This directly addresses "the agent changed things I didn't ask for".

**Reuse before writing.** The system prompt requires the agent to consult `lib.index` and `fs.search` before defining a new function, and to extend or call an existing library function when one covers the need. The checker enforces the result (5.13).

**Capability baseline.** The loop requires models with native tool calling and targets streaming and a context window of at least 32k tokens; this is the definition of a "performant model" for this product. A model that fails the tool-calling probe (5.10) is rejected with an error that says so; there is no text-protocol fallback.

### 5.8 Policy engine and edit gating (D7)

**Decided:** `review` is the default gating mode, review is per hunk, and protected paths are in the first version. Comment-marker locked regions inside a file were removed (DESIGN-REVIEW item 5): protect the file's path instead.

The engine is the shared crate `wb-policy`; `crates/core/src/tools/gate.rs` only maps a tool call onto it. The policy surface is three presets plus protected paths:

| `preset` | Edits | Runs | Packages, shell, git |
|---|---|---|---|
| `review` (the default values) | every edit reviewed | auto | ask |
| `auto` | applied inside the declared scope, asked outside it | auto | ask |
| `read-only` | denied | auto (existing scripts) | denied |

`preset` expands to the detailed keys below; a detailed key written in the same table overrides the preset's value. The settings dialog offers the three presets and keeps the detailed keys under Advanced. Whatever the preset: paths under `.git/` and `.analyst/` and protected globs are never editable (also through a symlink), runs never get network access, and when no OS sandbox confines runs every run, snippet and shell command asks first unless the project sets `unsandboxed_runs = "auto"`.

Policy is declarative, lives in `.analyst/project.toml`, and can be overridden per session from the UI without editing the file.

```toml
[policy]
preset   = "review"        # review | auto | read-only; expands to the five keys below
edits    = "review"        # review | auto-new-files | auto | deny
runs     = "auto"          # auto | ask
packages = "ask"           # ask | auto | deny
shell    = "ask"           # ask | auto | deny
git      = "ask"           # commits by the agent
protected = ["analysis/core/**", "data/**"]   # never editable by the agent
max_steps = 40
```

Edit gating states:

| Mode | Behavior |
|---|---|
| `review` | The turn pauses on each proposal. The diff appears inline in the editor (accept or reject per hunk) and as a card in chat. The turn resumes with the accepted content. |
| `auto-new-files` | New files are written immediately; modifications to existing files pause for review. |
| `auto` | All in-scope edits apply immediately. Out-of-scope edits still pause. A checkpoint exists for every turn. |
| `deny` | No edit is applied or offered for review (the `read-only` preset). |

Conflict detection: a proposal carries the content hash of the file it was based on. If the user changed the file meanwhile, the proposal is rejected with a conflict and the agent must re-read.

### 5.9 Checkpoints

Before every turn that could change files, the core writes the local working tree (including untracked, excluding ignored) to a tree object and stores it under `refs/analyst/checkpoints/<turn-id>` using git plumbing (`git add -A` into a temporary index, `git write-tree`, `git commit-tree`). This never touches the user's index, branches, or history, and these refs are never pushed. "Revert this turn" restores the tree and marks the remote mirror stale. Checkpoints are pruned by age and count.

The same timeline covers decks: slide documents are files in the tree, and the deck service also checkpoints before an export (the export records its checkpoint), before a template is replaced, and before a switch to code mode (switching back restores the deck's `deck/` from that checkpoint). A restore first checkpoints the state it replaces, so it can itself be undone. This replaces the deck workbench's snapshot store. `analyst deck history` and the `history_*` commands (IPC.md section 9) list and restore it.

### 5.10 Model provider layer (D4)

**Decided: model agnostic.** No prompt text, tool schema, or code path is conditioned on a model name. The loop assumes the capability baseline in 5.7 and adapts only through a probed capability record.

One internal interface:

```rust
trait ModelClient {
    fn stream(&self, req: ChatRequest) -> impl Stream<Item = ModelEvent>;  // TextDelta | ToolCall | Usage | Stop
}

struct ModelCapabilities {
    context_window: u32,
    tools: ToolSupport,      // Native (required) | None
    vision: bool,
    probed_at: Timestamp,
}
```

Two wire adapters cover every provider you named and most others:

| Adapter | Covers |
|---|---|
| OpenAI-compatible chat completions (streaming, tools, images) | Ollama (`/v1`), vLLM, llama.cpp server, LM Studio, OpenAI, OpenRouter, Mistral, DeepSeek, Groq, Gemini's OpenAI-compatible endpoint |
| Anthropic Messages (streaming, tools, images) | Anthropic |

An optional Ollama-native adapter adds model listing and pulling from the UI; it is not needed for chat.

**Capability probing.** On first use of a model (and on demand) the core runs a short probe: one exchange requiring a tool call with a known argument, one with an image if the user allows it, and a query of the provider's model-info endpoint (`/v1/models`, Ollama `/api/show`) for the context window. Results are cached in the capability record with a timestamp. Manual overrides are allowed but not required. This replaces any hand-maintained per-model list.

Provider configuration in `~/.analyst/config.toml`:

```toml
[[providers]]
name = "ollama-lab"
kind = "openai-compatible"
base_url = "http://lab-gpu:11434/v1"
# api_key: env var OLLAMA_LAB_API_KEY (the derived default name), or the same name in a .env file

[[providers]]
name = "anthropic"
kind = "anthropic"

[[models]]
provider = "ollama-lab"
id = "<any model served there>"
# capabilities are probed; override only if the probe is wrong, e.g. context_window = 65536
```

API keys are named by an environment variable: `api_key_env`, or the derived default `<PROVIDER NAME>_API_KEY` (uppercased, non-alphanumerics to `_`). The lookup order is the process environment, then `~/.analyst/.env` (mode 0600, written by the settings dialog and `analyst key set`), then the repo's own `.env` when it exists and git does not track it. There is no keychain and no `credentials.toml`. Every provider request is built by the network gate (5.6), which only allows the hosts of configured providers. The chat header always shows which provider is active and whether it is local or remote, so you know what leaves the machine.

Model choice is per conversation. A later feature allows different models for roles (planner, coder, summarizer); the interface already supports it.

### 5.11 Equation lens (D8)

The goal is to show the math a function implements, derived from the code wherever possible, and to make the gap between "declared" and "implemented" visible when derivation is not possible.

Three provenance tiers, each shown with a distinct badge:

**Tier A, derived.** For functions whose body is a sequence of assignments ending in a return, composed of arithmetic, powers, NumPy and `math` element-wise functions (`exp`, `log`, `sqrt`, `sin`, ...), constants, and references to parameters, the kernel transpiles the AST into a SymPy expression without importing or executing the user's module. Assignments are substituted, the result is simplified lightly and rendered to LaTeX. Array indexing, loops, conditionals, and calls to unknown functions end the derivation for that function (it falls to tier B). This tier is ground truth by construction.

**Tier B, declared and checked.** The function's docstring carries a `Math:` section in SymPy-parsable syntax:

```python
def double_exp(t, A, tau1, B, tau2):
    """Two-component exponential decay.

    Math:
        y = A*exp(-t/tau1) + B*exp(-t/tau2)
    """
    return A * np.exp(-t / tau1) + B * np.exp(-t / tau2)
```

The kernel renders the declaration to LaTeX with SymPy and verifies it numerically: it lambdifies the declared expression, calls the real function in the session venv on random inputs within declared or default ranges, and compares within tolerance. Badge: green (agrees), red (mismatch, with the inputs that disagree), grey (could not evaluate). A `LaTeX:` section is also accepted for display-only material (derivations, algorithm statements) and is always marked unverified.

**Tier C, explained.** On request, the agent writes an explanation with equations for a function or algorithm that neither tier A nor B covers. Always marked unverified and stored outside the source file.

Surfaces:

- **Equations panel:** every equation in the current file or module, rendered with KaTeX, with badge, function name, and line. Click to jump. Filter by tier or status.
- **Editor decorations:** a code-lens line above each covered function ("view equation", badge).
- **Agent tools:** `eq.extract` and `eq.check` so the agent verifies its own work and the script contract can require `Math:` sections on functions in designated modules.
- **Slides:** "Place on slide" (and the agent's `slide.place_equation`) puts the equation on the selected slide as an equation element carrying its equation record (LaTeX, tier, numeric check with trials, tolerance and counterexample, function, file, the file's content hash). The slide preview shows it with KaTeX and a tier/check badge that opens the Equations panel at the function; the exported `.pptx` holds a native, editable PowerPoint equation (OMML, converted in the deck SDK; an image with alt text when the LaTeX is outside the converter's subset). The checker reports a mismatch or LaTeX-only equation as an error unless acknowledged, and a changed source file as `stale`.

The `Math:` docstring convention is deliberately dependency-free: the user's repo needs no package from the app. Extraction runs in the tooling environment on the target; numeric checks run in the session venv there.

### 5.12 Figure pipeline

Capture happens in the script's process through a Matplotlib backend injected by the kernel (`MPLBACKEND=module://analyst_mpl`, module on `PYTHONPATH`). It overrides `show()` and wraps `savefig()`:

- each figure is written to `out/figures/<script>/<run-id>/<name>.png` and `.svg` (optionally a pickle of the Figure for re-styling later; the pickle stays on the target)
- a sidecar JSON records script, arguments, run id, target, figure title, axes labels, and timestamp
- the kernel watches the run's figure directory and emits `run.artifact` events; on a remote target the core pulls the files into the local `out/`
- figures on this machine reach the renderer through the asset protocol (`FigureView.file_abs`; the session's output directory and pulled-artifact folder are in the asset scope); `figure_data` (base64) remains for remote figures not yet pulled

Terminal runs get the same treatment two ways: the app's own terminals export the capture environment automatically (`source .analyst/env.sh` does the same in any other shell), and the kernel's watcher on `out/figures/**` picks up anything else.

Figures panel:

- tabs per figure, grouped by run; latest run of a script replaces the tab content by default, with a history slider to go back
- split view to compare two runs of the same figure, including runs from different targets
- pin a figure so it survives new runs
- undock into its own OS window (section 6)
- "send to agent" attaches the image to the next message for vision-capable models, or the sidecar metadata for others

Phase 1 captures static Matplotlib output only. Interactive figures (Plotly, Bokeh HTML) are a later phase.

**Artifact index and run citations.** The core indexes what each run of the session produced from the run records and the artifact lists the kernel writes (no output folder is scanned): run id, path, kind, script, arguments, and the tree identity the run executed (`artifacts.list`, `artifacts_list`). `slide.place_figure` copies a figure into the deck's `assets/` by content hash (data files are refused) and the image element carries a `run_cite` (run id, artifact, script, arguments, tree identity). The deck checker reports an element whose run citation no longer matches the working tree's identity as `stale` (a warning). Decks are outside the tree identity, so editing slides does not make figures stale. This replaces the deck workbench's scan/import bridge. Scripts can also record named numeric results (`analyst_results.record(name, value, unit, uncertainty)` writes `<out>/results.json`, artifact kind `results`); slide text binds a number to one through a run citation on the text run, and the checker verifies the displayed number against the recorded value (`num.bound`) and reports a changed tree as `stale`. **Refresh from analysis** brings a stale figure, equation, or bound number up to date without the agent: the cited script is re-run with the cited arguments through the run service, and the updated slide is a normal proposal (before/after card listing the changed values and the verifications it invalidates).

**Verification ledger.** `.analyst/verification.jsonl` (append-only, committed with the repository) records per item (equation, figure, named result, slide claim, accepted code change) the content hash a state applies to: `machine_checked`, `human_verified`, or `flagged`, with who, when, and a note. An item whose current content has no entry is `unverified`, so any change to the code, a run's output, or a slide resets what was verified. Only the user records `human_verified`; the agent's `verify.record` can record `machine_checked` and is allowed under every preset. Exporting a deck to pptx or pdf first lists, in a confirm dialog, what it depends on that is not human-verified (`deck_verification_gaps`).

### 5.13 Script contract and checker (D9)

The contract encodes the conventions you enforce by hand today. It is both a prompt (the agent is told the rules) and a checker (the kernel verifies them with AST analysis). Two rules are confirmed as the ones you currently enforce manually and are hard failures by default; the rest are proposals pending OQ-6.

**Confirmed rule 1: every script is a command-line tool.**

- A script builds an `argparse` parser with a description and help text on every option, defines `main(args)`, and has an `if __name__ == "__main__":` guard.
- Every input (data path, parameter, output location, seed, plot options) is an option with a default. Defaults for paths are repo-relative.
- No input is hard-coded: a literal path or numeric constant that flows into a library call from `main` without passing through the parser is a violation.

Checker rules: `cli.parser-required`, `cli.help-text`, `cli.main-guard`, `cli.option-for-every-input`, `cli.no-absolute-paths`.

**Confirmed rule 2: shared logic lives in the library, not in scripts.**

- Computational code lives in an importable package (`analysis/` by default) as documented functions. Scripts are thin: parse arguments, call library functions, save outputs.
- A script may define only `main`, `build_parser`, and small private helpers (configurable: default under 15 lines, no numerical work). Anything else must be in the library.
- No two functions in the repo may implement the same logic. Before writing a function the agent must check the library index.

Checker rules:

| Rule | Detection |
|---|---|
| `lib.script-defines-logic` | A script defines a function outside the allowed set, or a helper exceeds the size limit or calls numerical libraries. |
| `lib.duplicate` | Two functions anywhere in `scripts/` or `analysis/` have the same normalized AST fingerprint (names and literals stripped, structure hashed). Hard failure. |
| `lib.near-duplicate` | Token-sequence similarity above a threshold (default 0.8) between two functions, or identical signature and name in a script and the library. Warning with both locations and a "promote to library" or "reuse" hint. |
| `lib.unused-twin` | A script calls a locally defined function while a library function with a matching name or signature exists. |

`contract.similar` exposes the same analysis on demand so the agent can run it before proposing an edit, and the UI offers "promote to library" as a refactor action that moves a function into `analysis/` and rewrites the call sites as a reviewable proposal.

**Default rules (accepted as defaults; severities adjustable per repo)**

3. Outputs go to `out/` under a subdirectory named after the script. Figures are saved with descriptive names. A run metadata JSON is written with the arguments used.
4. Random seeds are options with defaults.
5. Functions in modules listed under `contract.math_required` carry a `Math:` docstring section.
6. Library functions have docstrings with parameter units where applicable.

Each rule has a severity (`error` or `warn`) in `.analyst/project.toml`. Checker output is a list of diagnostics with file, line, rule id, and a fix hint, shown in the editor and fed back to the agent after each edit.

### 5.14 Documents

- **Markdown:** edited in the main editor, previewed live in a panel using `react-markdown` with `remark-math` and KaTeX. Figure embeds support both plain image paths and run references such as `{{fig: fit_decay/residuals}}` that resolve to the latest run, so notes stay current as analyses are rerun.
- **LaTeX:** `.tex` files compile with a TeX installation the user provides (Tectonic and its on-demand package fetching are removed from the plan) to PDF, shown in a `pdf.js` viewer panel. Compile on save with a debounce. Errors are parsed into editor diagnostics. SyncTeX click-to-source later. Compilation is always local.
- **Agent support:** `doc.insert_figure` and `doc.render` tools; a "report" skill that knows the document structure.

Each session gets a `notes.md` by default.

### 5.15 Terminal

`xterm.js` in the renderer over a `portable-pty` in the core, opened in the repo root with the session venv activated and the figure-capture environment set. The shell has no network access: it is started through the no-network wrapper (`bwrap --unshare-net` or `sandbox-exec`), and the panel shows "no network" or, where no isolation is available, "network not isolated". On a remote target the pty runs `ssh -t` into the mirror directory with the same setup, wrapped with `bwrap` on the host when it works there. Multiple terminals per session. This is the manual door into the same loop: anything run here produces figures and run records like an agent run.

### 5.16 Skills and configuration

Skills are Agent Skills folders, loaded by the shared crate `wb-skills`: `<name>/SKILL.md` with `name` and `description` in the front matter, plus any resource files.

```markdown
---
name: curve-fitting
description: How to fit models and report uncertainties.
---
When fitting models: report parameter uncertainties from the covariance, plot residuals, ...
```

Skills load from `~/.analyst/skills/` (user) and `<repo>/.analyst/skills/` (project; it wins on a name clash). The system prompt lists them by name and description; the agent loads one in full with the read-class tool `skill.load` before relying on it.

The earlier front matter keys keep working inside `SKILL.md`: `triggers` (words in the user message), `paths` (globs of files in the declared scope or changed this turn) and `always: true`. A skill that carries them is put into the prompt in full when it matches the turn, without a tool call. Flat `<name>.md` files, which the earlier design described and no version loaded, are not read. A toggle in the UI for the skills of a turn is not built.

Configuration layering, lowest to highest precedence: built-in defaults, `~/.analyst/config.toml`, `<repo>/.analyst/project.toml`, per-target overrides inside it, session overrides from the UI.

**Migration from `~/.present`.** On first start the CLI and the desktop shell merge an existing `~/.present` into the analyst home once: its `[[providers]]` join `config.toml` (names that already exist are skipped; `key = "env:VAR"` becomes `api_key_env = "VAR"`), `skills/` and `templates/` are copied without overwriting, keys from its `.env` that are not set in `~/.analyst/.env` are copied, and the marker `.present-migrated` makes it run once. `~/.present` is never changed or deleted. `PRESENT_HOME` is no longer read (`crates/core/src/migrate.rs`).

## 6. User interface

One window, one renderer (`apps/desktop/src`), one dock for both workbenches. The layout follows Zed's shape so the muscle memory transfers, with the analysis panels and the deck panels added to the same `dockview` instance; the agent chat, command palette and keymap, settings, provider and model picker, and theme are shared.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ [decay-analysis · lab-gpu ●] [beam-sim · local] [+]          model ▾  ⚙ │
├──────────┬──────────────────────────────────────┬────────────────────────┤
│ Project  │ Editor tabs: fitting.py | fit_decay.py│ Agent chat            │
│ Scripts  │ ─────────────────────────────────────│ mode: Build ▾         │
│ Library  │   def double_exp(t, A, tau1, ...):   │                       │
│ Runs     │   ∑ y = A e^{-t/τ₁} + B e^{-t/τ₂} ✓  │ [plan card]           │
│ Skills   │       return A * np.exp(-t/tau1) ... │ [diff card ✓ ✗]       │
│          │                                      │ [run card · lab-gpu]  │
│          ├──────────────────────────────────────┤                       │
│          │ Terminal | Run output | Diagnostics  │                       │
├──────────┴──────────────────────────────────────┴────────────────────────┤
│ Figures ◧ | Equations | Packages | Preview | (dockable, undockable)      │
└──────────────────────────────────────────────────────────────────────────┘
```

Panels and their primary interactions:

| Panel | Interactions |
|---|---|
| Session tabs | Switch, create, close, duplicate with a new conversation. Each shows its target and connection state and restores its full layout. |
| Project sidebar | File tree. Scripts view lists each script with its options parsed from the index and a form to run it with chosen arguments. Library view lists public functions with callers and "promote to library" candidates from the checker. Runs view with history, target, and filters. |
| Editor | CodeMirror 6 with Python syntax, Ruff diagnostics, Inline diff review of agent proposals. Code-lens for equations. Protected-region markers. |
| Agent chat | Streaming messages. Cards for plans, proposals, runs, package changes, and approvals. Mode switch. Model switch. Context indicator (tokens used, compaction state). Attach files, figures, or editor selections. |
| Figures | Section 5.12. |
| Equations | Section 5.11. |
| Packages | Installed list on the session's target, search and add, remove, sync, Python version. Pending agent requests show here too. |
| Preview | Markdown or PDF. Follows the active document. |
| Terminal / Run output / Diagnostics | Bottom dock. Run output shows structured runs with stdout, stderr, target, and artifact links. |
| Decks | Sidebar view: the session's decks (`decks/<slug>/`) and the New deck dialog (title, template file from the native open dialog, structured or code mode). Opening a deck shows it in the deck panels below; they follow the session's active deck. |
| Slides | The slide rail: live-preview thumbnails (the fidelity render when current), check and number badges, locks, drag to reorder, add, duplicate, delete. |
| Canvas | The selected slide drawn from the deck document and template description; click a text element to edit it in place (`slide_update` with the expected hash), layout picker, lock toggle, reference citation markers (open Sources), run citation badges and stale markers. |
| Outline | The storyline as an editable table: action title (the slide title), key message (`.present/outline.json`), layout, sources (references and runs). |
| References / Sources | Reference list with extraction status, add through the native dialog, remove unused, search; Sources shows the passage, table cells, or page image behind a clicked citation. |
| Checks / Notes / Exports | Deck checker findings by slide and rule (click to select the slide), "Refresh all stale"; speaker notes of the selected slide; export pptx, pdf, png or outline (pptx and pdf warn about items not verified by the user), export history, reveal and open. |
| Verify | The verification queue: every item not verified by the user, used on slides first, grouped by kind; `j`/`k` move, `v` marks verified, `f` flags with a note, `Enter` opens the item in Equations, Figures, Runs, or Canvas. Slide elements, figures, and equations show their verification state. |

**Both workbenches in one control loop.** The one agent chat has six modes (plan, build, run only, outline, slides, polish); slide proposals render as before/after cards drawn with the canvas's renderer, next to code diff cards, with accept, reject, and accept-modified through `proposal_respond`; deck check and export results show as cards. **Verification is one click away:** an image element placed from a run carries a `run_cite`, and its badge selects that run in the Runs view, the figure in the Figures panel, and the run output with its script and arguments; a `stale` finding marks the element and lists in Checks. The Figures panel's "Place on slide" puts the shown figure on the selected slide as a user action (`slide_place_figure`). **Mixed docking:** every panel of either workbench can be dragged into any group, so a slide can sit beside its source figure and equation, or the editor beside the slide it feeds; the layout is saved per session. The renderer holds no authority: every change goes through the commands of IPC.md, and deck images load through the asset protocol (deck caches, assets, references in the scope) with no network in the renderer.

Docking uses `dockview`: any panel can be dragged into any dock group or into a floating group. Undocking into a separate OS window creates a second Tauri `WebviewWindow` hosting that panel, subscribed to the same session's event channel; this is what makes a figures-only window on a second monitor work.

Command palette and keybindings follow Zed's defaults (decided). A Vim mode via CodeMirror's `@replit/codemirror-vim` is a phase 4 item.

## 7. Storage layout

```
~/.analyst/                              (local machine)
  config.toml              providers, models, targets, global policy defaults, UI prefs
  skills/                  global skills
  .env                     API keys saved through the app (mode 0600)
  egress.jsonl             ledger of every network decision (destination, purpose)
  tooling/                 app-owned Python environment for the local target
  ssh/                     control sockets for multiplexed connections
  .present-migrated        marker: ~/.present was merged in once (providers, skills/, templates/, .env keys)
  templates/               deck templates (also from the ~/.present migration)
  sessions/<id>/
    decks/<slug>/          per-user deck state: reference index, export list
    session.toml           repo path, target, env path, layout, active model
    conversations/<id>.jsonl
    runs.jsonl             run records
    meta.json, layout.json conversations, policy overrides, dock layout

~/.analyst/                              (each remote target)
  bin/uv
  tooling/                 kernel, SymPy, Ruff, language server
  runs/<run-id>/           stdout, stderr, status, artifact list (survive disconnects)

<repo>/                                  (local; mirrored to <remote_root>/<repo> on remote targets)
  .analyst/
    project.toml           contract, policy, data dirs, per-target overrides   (committed)
    skills/                project skills                                      (committed)
    .env                   optional API keys for this repo                      (ignored; never synced)
    env.sh                 generated; figure capture env for terminals          (ignored)
  scripts/  analysis/  data/  out/  notes.md  pyproject.toml  uv.lock
  decks/<slug>/            deck/ template/ references/ assets/ .present/project.toml   (committed)
                           exports/ .present/cache/                                     (ignored)
```

`.analyst/project.toml`, `pyproject.toml`, and `uv.lock` are meant to be committed so a repo carries its own conventions and environment. Everything under `out/`, `.venv/`, and `.analyst/env.sh` is git-ignored by the template and excluded from sync.

## 8. Security and privacy

- No telemetry. No network calls except to configured model providers, the configured package index during installs, and configured SSH targets, all through the gate described in 5.6 and all recorded in `<home>/egress.jsonl`.
- API keys in the environment or an untracked `.env` file (`~/.analyst/.env`, mode 0600, or the repo's). Never logged, removed from the environment of kernels and terminals, never passed into the sandbox, never sent to a remote target (a repo `.env` is excluded from the mirror), and not readable through the agent's file tools.
- SSH credentials are never handled by the app; it relies on the user's OpenSSH configuration and agent.
- The chat UI shows provider locality (local or remote) at all times. A per-target "data directories" list controls what the agent can read; nothing outside the repo and that list is readable by tools or by the sandbox.
- Transcripts stay local. Export is explicit.

## 9. Cross-platform (D16)

macOS and Linux are the supported platforms through phase 3, for both the desktop app and remote targets. Tauri builds for Windows from the same codebase, but Windows is not tested or shipped until phase 4; when it is, it will use the built-in OpenSSH client without connection multiplexing, the `tar` sync fallback when `rsync` is absent, and no local sandbox. `uv`, `rg`, and Tectonic are bundled or fetched per platform.

## 10. Testing and evaluation (D15)

- **Unit and integration:** core crates with `cargo test`, kernel with `pytest`, IPC contract tests generated from the `protocol` crate, sandbox tests that assert denied writes and denied network, SSH tests against a containerized `sshd` covering bootstrap, sync, detached runs, and reconnect.
- **Agent evaluations, model-agnostic by design.** A suite of representative analysis tasks (fit a model, process a dataset, produce a figure, refactor a script to the contract, extend an existing library function instead of duplicating it) with automatic graders that inspect only the resulting repo and runs: contract passes, run succeeds, expected artifacts exist, declared equations verify, no edits outside the declared scope, no duplicate logic introduced. The harness takes any configured provider and model and emits a scorecard: task success rate, contract compliance, equation verification rate, unwanted-edit rate, tool-call error rate, steps and tokens to completion. No task or grader references a model; the suite is a regression test for prompts and tools and a compatibility report for whichever models you point it at.
- **UI:** Playwright against the renderer with a fake core for determinism; Tauri's WebDriver harness for smoke tests on each platform.

## 11. Repository structure and build order

```
apps/desktop/
  src-tauri/           Tauri shell: commands, events, window management (thin glue over crates/core)
  src/                 React renderer
crates/protocol/       IPC and kernel message types; TypeScript bindings generated with ts-rs
crates/wb-*/           shared with the deck workbench: network gate, providers, policy, transcript, skills, sandbox
crates/core/           sessions, agent loop, tools, targets, sync, storage (providers, policy, transcript and skills come from crates/wb-*)
crates/cli/            the `analyst` CLI: analysis commands, `deck ...`, `chat`, `eval` (analysis and deck tasks)
present/crates/core/   analyst-deck: deck store, references, deck kernel client, deck tools, per-deck service
present/crates/protocol/ analyst-deck-protocol: deck documents, deck kernel and UI types (ts-rs into apps/desktop/src/ipc/generated/deck)
present/python/        the deck kernel (present_kernel) and the deck build SDK (present_sdk)
python/kernel/         analyst_kernel (runs, index, equations, contract) and analyst_mpl
evals/                 task suite, graders, scorecard runner
docs/
```

Build order follows the roadmap in the vision document. Phase 0 produces `crates/core` (without UI glue), `python/kernel`, the local and SSH transports, and the CLI harness. That phase answers the riskiest questions (remote sync and reconnect behavior on a login node, sandbox behavior on real hosts, equation derivation coverage, and how the loop performs across several models from different providers) before any interface work. All decisions that block phase 0 are closed; see OPEN-QUESTIONS.md for what remains open for later phases.
