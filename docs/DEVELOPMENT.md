# Development

Personal Analyst is one product: one core, one desktop app, and one `analyst` CLI for analysis and decks (the former Personal Present lives on as the deck library under `present/`). See [ARCHITECTURE.md](ARCHITECTURE.md) for the design and [PROTOCOL.md](PROTOCOL.md) for the contract between components.

## Layout

```
Cargo.toml             Rust workspace
crates/wb-*/           shared with the deck workbench: wb-net (network gate), wb-providers (model clients, capability
                       probe), wb-policy (policy engine, presets), wb-transcript (events, JSONL store, replay, compaction,
                       pinned facts), wb-skills (Agent Skills loader), wb-sandbox
crates/protocol/       serde types for the kernel protocol, config, typed transcript events for the UI, evals
crates/core/           sessions, agent loop, tools, kernel client, targets, sync, checkpoints, the IPC command table
                       (commands.rs); its provider, policy, transcript and skills code is the shared crates plus
                       thin adapters (model_client.rs, tools/gate.rs, session.rs, skills.rs)
crates/evals/          analyst-evals: the analysis and deck evaluation harnesses (used by the CLI only)
crates/serve/          analyst-serve: development bridge, the real core over a loopback WebSocket (never shipped)
crates/cli/            the `analyst` binary (analysis commands, `deck ...`, `chat`, `eval`); its start-up is
                       `analyst_core::app::App::start`, shared with the shell and analyst-serve
present/crates/core/   analyst-deck: deck store, references, deck kernel client, deck tools, per-deck service
present/crates/protocol/ analyst-deck-protocol: deck documents, deck kernel and deck UI types
python/kernel/         analyst_kernel (per-target kernel, its REPL process and its equation worker) and
                       analyst_mpl (figure capture backend)
present/python/        present_kernel (the deck kernel) and present_sdk (deck build scripts)
evals/tasks/           model-agnostic analysis evaluation tasks with fixture repos
present/evals/         deck evaluation tasks and templates, run by the same `analyst eval run`
tools/deck_walkthrough.sh  the deck commands end to end against the scripted model server
docs/                  vision, architecture, protocol, decisions
```

## Prerequisites

- Rust stable (1.80 or newer), `cargo`
- Python 3.11 or newer, `uv`, `ruff`
- `git`, `rg` (ripgrep)
- For sandboxed runs: `bwrap` (Linux) or macOS. Without either the kernel falls back to resource limits and reports `sandbox: "limits"`.
- For remote targets: OpenSSH client and `git` locally; `git`, `tar`, and the target's Python version on the host (the app installs neither `git` nor Python there)
- For the SSH sync integration test (`crates/core/tests/sync_ssh.rs`): `sshd`, `ssh`, and `ssh-keygen` (`apt-get install -y openssh-server openssh-client`). The test starts its own server on a localhost port with throwaway keys and is skipped when they are missing; `ANALYST_REQUIRE_SSHD=1` turns the skip into a failure

## Setup

```sh
# Python kernel and its tests
cd python/kernel
uv sync            # creates python/kernel/.venv with sympy, ruff, pytest, numpy, matplotlib
uv run pytest -q
uv run ruff check .
cd ../..

# Rust workspace
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Running the CLI in development

The CLI resolves the tooling Python (the interpreter that runs the kernel) in this order: `ANALYST_TOOLING_PYTHON`, `tooling_python` in `~/.analyst/config.toml`, `python/kernel/.venv/bin/python` relative to the workspace (dev mode), then `python3` on `PATH`. After `uv sync` in `python/kernel`, dev mode works with no configuration.

```sh
cargo run -q -p analyst-cli -- init /tmp/demo        # new analysis repo with venv
cd /tmp/demo
analyst index                                        # scripts and library index
analyst check                                        # contract + lint
analyst run scripts/example.py -- --help
analyst eq analysis/models.py --check
analyst chat -m <provider>/<model> "..."             # needs a provider in ~/.analyst/config.toml
analyst eval run --model <provider>/<model> --tasks <workspace>/evals/tasks

# decks: decks/<slug>/ in the same repository (a plain directory becomes a project first)
analyst deck init q3 --template corp.potx --title "Q3 review"
analyst deck refs add sales.xlsx                     # --deck <slug> when there are several
analyst deck check                                   # build + deck checker (exit 2 on errors)
analyst deck render --out exports/draft.pptx
analyst deck export --format pdf                     # pptx, pdf, png, md, notes-docx; checkpointed first
analyst deck eject && analyst deck build             # code generation mode
analyst deck history list                            # git checkpoints: code and decks together
analyst deck history revert-turn <conversation>-<n>  # as `analyst chat` prints it
analyst deck template inspect corp.potx --catalog
```

`analyst chat` offers the deck tools whenever the repository has a deck; `--mode` takes `plan`, `build`, `run`, `outline`, `slides`, or `polish`. Deck kernels need `present/python/kernel/.venv` (`uv sync` there) in a checkout, or `PRESENT_RUNTIME_PYTHON`. Tests and the walkthrough set `PRESENT_FONT_DIRS=$PWD/present/vendor/fonts` (the variable keeps its name).

**`~/.present` migration.** The first `analyst` command (or desktop start) merges an existing `~/.present` into `$ANALYST_HOME` once (providers, `skills/`, `templates/`, missing `.env` keys) and writes `$ANALYST_HOME/.present-migrated`. `PRESENT_HOME` was removed and is not read; there is no `present` binary.

Provider configuration lives in `~/.analyst/config.toml` (schema in PROTOCOL.md section 2.1). API keys are read from the environment variable named by `api_key_env` (default `<PROVIDER NAME>_API_KEY`), then from `$ANALYST_HOME/.env` (`analyst key set <provider>` writes it), then from an untracked `.env` in the repo. `[packages] index_url` sets the package index (default `https://pypi.org/simple`). Override the home directory with `--home` or `ANALYST_HOME`.

## Evaluation suite

Each task under `evals/tasks/<name>/` has a `task.toml` (prompt, mode, checks) and a `fixture/` directory that becomes a fresh git repo for the run (with `uv sync` for its `pyproject.toml`). Checks inspect only the resulting repo, the kernel's reports, and the transcript; none reference a model. Results go to `evals/results/`.

```sh
analyst eval run --model <provider>/<model> --tasks evals/tasks --auto            # all tasks
analyst eval run --model <provider>/<model> --tasks evals/tasks --task parameter-sweep --auto
```

### Exercising the loop without a model

`evals/fake_model_server.py` is a scripted OpenAI-compatible server (stdlib only). It answers the capability probe, plays the `parameter-sweep` task (three runs, no edits) and the `reuse-library-function` task (scope declaration, file creation, run), and declines everything else. It drives the real provider adapter, agent loop, policy engine, tools, kernel, graders, and scorecard, so it is the integration test for the whole headless stack and runs in CI.

```sh
python3 evals/fake_model_server.py --port 8765 &
mkdir -p /tmp/analyst-home && cat > /tmp/analyst-home/config.toml <<'EOF'
[defaults]
model = "fake/scripted"
[[providers]]
name = "fake"
kind = "openai-compatible"
base_url = "http://127.0.0.1:8765/v1"
EOF
ANALYST_HOME=/tmp/analyst-home analyst eval run --model fake/scripted --tasks evals/tasks --task parameter-sweep --auto
ANALYST_HOME=/tmp/analyst-home analyst eval run --model fake/scripted --tasks evals/tasks --task reuse-library-function --auto
```

Both must report PASS. The other two tasks fail by design against the scripted server; they need a real model.

Deck tasks run through the same command and the one loop; `present/evals/fake_model_server.py` scripts two of them:

```sh
python3 present/evals/fake_model_server.py --port 8766 &
mkdir -p /tmp/deck-home && printf '[defaults]\nmodel = "fake/scripted"\n[[providers]]\nname = "fake"\nkind = "openai-compatible"\nbase_url = "http://127.0.0.1:8766/v1"\n' > /tmp/deck-home/config.toml
cd present
ANALYST_HOME=/tmp/deck-home PRESENT_FONT_DIRS=$PWD/vendor/fonts analyst eval run --tasks evals/tasks --task spreadsheet-to-charts
ANALYST_HOME=/tmp/deck-home PRESENT_FONT_DIRS=$PWD/vendor/fonts analyst eval run --tasks evals/tasks --task fix-checker-failures
cd .. && tools/deck_walkthrough.sh                  # every deck command, with its own temporary home
```

Both deck tasks must PASS; the other six deck tasks need a real model.

## Desktop app (phase 1)

The workbench is a Tauri 2 application: `apps/desktop/src-tauri` (Rust: an invoke handler over the one command table `analyst_core::commands`, which the development bridge uses too) and `apps/desktop/src` (React). The IPC contract is [IPC.md](IPC.md); TypeScript types are generated from `crates/protocol` by `cargo test -p analyst-protocol` into `apps/desktop/src/ipc/generated/`, the command table `src/ipc/commands.ts` from `crates/core/src/commands.rs` by `cargo test -p analyst-core --lib commands` (that test fails when the committed file was stale; run it and commit the result), and the deck types from `present/crates/protocol` by `cargo test -p analyst-deck-protocol` into `apps/desktop/src/ipc/generated/deck/`. The shell embeds `apps/desktop/dist`, so run `pnpm build` in `apps/desktop` before `cargo clippy` or `cargo test -p analyst-desktop`. There is one renderer; the deck workbench's panels are part of it (`src/deck/`, `src/ui/deck/`, `src/state/decks.ts`).

```sh
cd apps/desktop
pnpm install
pnpm typecheck && pnpm lint && pnpm test && pnpm build
pnpm test:e2e         # Playwright, headless Chromium, against the real core (see below); build first

# the real app (needs the Python kernel set up as above)
cargo tauri dev                                   # from apps/desktop/src-tauri; starts Vite and opens the window
cargo build -p analyst-desktop --features custom-protocol   # debug binary with the built frontend embedded
cargo tauri build                                 # installers (.app/.dmg, .deb/.rpm/.AppImage)
```

### The renderer in a browser, against the real core

`analyst-serve` hosts the same core as the app and speaks its commands and events over a WebSocket on 127.0.0.1 (IPC.md section 11). It prints its URL, with a fresh token, on start; the renderer takes it as `?serve=` or `VITE_ANALYST_SERVE`:

```sh
cargo run -q -p analyst-serve                       # analyst-serve: listening on ws://127.0.0.1:41234/?token=3f9c...
cd apps/desktop
VITE_ANALYST_SERVE='ws://127.0.0.1:41234/?token=3f9c...' pnpm dev
# or open http://localhost:5173/?serve=<the URL, percent-encoded>
```

It uses `ANALYST_HOME` like the app (point it at a scratch home to keep your own sessions apart), binds loopback addresses only, and refuses connections without the token. `--bind 127.0.0.1:<port>` fixes the port, `--token` (or `ANALYST_SERVE_TOKEN`) fixes the token, and `--pick-dir` / `--pick-file` answer the native dialogs it cannot show. It is a development tool: the shipped app never links it.

The Playwright suite (`apps/desktop/e2e/`) uses it the same way: `e2e/harness.ts` builds nothing, so first `cargo build -p analyst-serve -p analyst-cli` (it looks in `target/debug`, or `ANALYST_BIN_DIR`), `uv sync` in `python/kernel` and `present/python/kernel`, and `pnpm build`. Each worker starts one bridge over a temporary home, the scripted model server (`evals/fake_model_server.py`, which also plays the e2e chat turns), and fixture projects made with `analyst init` from the parameter-sweep eval fixture and `e2e/fixture/`; decks are made from a copy of `present/evals/templates/corp/corp.potx`. Before each test the open sessions are closed and the projects reset to their fixture commit (their `.venv` is kept).

Environment for the real app: `ANALYST_HOME` (default `~/.analyst`), `ANALYST_TOOLING_PYTHON` (a dev checkout auto-detects `python/kernel/.venv`), `ANALYST_SANDBOX` (override the sandbox kind for all runs), `RUST_LOG`.

Linux build dependencies: `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev libxdo-dev patchelf`. macOS needs Xcode command line tools only.

### Headless smoke test of the real app

`apps/desktop/smoke/run.sh <repo>` launches the real binary under Xvfb through `tauri-driver` and WebKitWebDriver, creates a session on the given repo, waits for the kernel, runs `scripts/decay_sim.py` from the Scripts form, waits for exit 0 and for its figure to render, and saves a screenshot. Requirements: `cargo install tauri-driver`, `apt-get install webkit2gtk-driver xvfb`, the embedded-frontend build above, and a repo with a venv (the `evals/tasks/parameter-sweep/fixture` files in a repo made by `analyst init` work).

## Troubleshooting

**A bubblewrap run fails with `Can't chdir to ...`** when the repo or the runs directory lives under `/tmp`: fixed by mounting the fresh `/tmp` before the specific binds; rebuild the kernel environment if you see it on an older checkout.


**`sandbox-exec: execvp() of '.../.venv/bin/python' failed: Operation not permitted` (macOS)**, or a run that dies immediately under `bwrap` on Linux. The sandbox hides the home directory, and the venv's `python` is a symlink to a base interpreter that `uv` keeps under `~/.local/share/uv/python`. The kernel resolves that symlink (and `home =` in the venv's `pyvenv.cfg`) and allows the base installation read-only, and it lists every path in both its given and canonical form (`/tmp` is `/private/tmp` on macOS). If a run is still denied:

The macOS sandbox also checks `file-read-metadata` on every directory along a path during lookup, so the profile grants metadata (stat only) on the chain of ancestors between your home directory and each allowed subtree; without that, an allow rule for a subtree under home has no effect. If a run is still denied:

1. Run it with `--sandbox limits` to confirm the script itself works: `analyst --sandbox limits run scripts/x.py`. This applies resource limits only and is badged as such.
2. Print and test the exact wrapper for your repo and interpreter, outside the CLI:
   ```sh
   python/kernel/.venv/bin/python -m analyst_kernel.sandbox --python /path/to/repo/.venv/bin/python --root /path/to/repo --run
   ```
   It prints the profile (or `bwrap` argv) and then executes a one-line Python command under it. On macOS, denials are also logged by the system: `log stream --style compact --predicate 'sender == "Sandbox"'` in another terminal shows the operation and path that was refused.
3. Add that path to `data_dirs` in `.analyst/project.toml` if it is data, or report it if it is part of the interpreter or a system location.

**`no Python 3.12 interpreter was found on ...`.** The app never downloads Python (`UV_PYTHON_DOWNLOADS=never` on every `uv` it starts, locally and on hosts). Install that version on the machine the message names, or for a remote target set `python = "<installed version>"` in its `[[targets]]` entry.

**`not installing: pyproject.toml names its own package source (...)`.** Installs come from `[packages] index_url` only. The message names the entry: an extra index, a `git` or `url` source, a direct URL requirement, or a lockfile package pinned to another registry. Remove it, or change `index_url` if that index is the one you mean to use. A `uv.lock` written against another index is refused the same way; delete it and let the next sync write a new one.

**`` `git` was not found on <host> ``.** The mirror on a remote target is a git checkout; install `git` on the host.

**Matplotlib cannot find a font cache or config.** Runs set `MPLCONFIGDIR` inside the run directory, which is writable; the user's `~/.matplotlib` is intentionally hidden.

## Network rule

The app reaches the network for model endpoints, package installs from the configured index, and SSH targets only, and only through `crates/wb-net` (ARCHITECTURE.md section 5.6). In `crates/core` that means: build HTTP requests with `wb_net::gate().request(..)`, never a client of your own; build `uv` installs with `egress::uv_install` (other `uv` calls run offline); and spawn command lines through the existing helpers (`target::run_command`, `TaskLogger::stream_command`, the kernel transports), which call `egress::guard`. `target::run_command` passes the working directory to the guard (`egress::guard_in`), which then also reads the project for package sources of its own before an install; code that spawns an install another way calls `egress::check_project_sources` itself. The only git command that may use the network is the mirror push built by `egress::git_push`. `python3 tools/check_egress.py` checks that no other crate can reach the network, and that no crate but the development bridge (`crates/serve`) depends on a server crate or is linked by the app. Decisions are logged to `$ANALYST_HOME/egress.jsonl`. Kernels and terminals are spawned through `wb_sandbox::no_network::wrap`; tests that need isolation skip when neither `bwrap` nor `sandbox-exec` works.

## Conventions

- Rust: `cargo fmt`, clippy clean, no `unwrap()` on I/O paths in library code.
- Python: Ruff clean, stdlib plus SymPy and Ruff only in the kernel; `analyst_mpl` is stdlib plus Matplotlib only.
- The kernel never imports user code in its own process; evaluation of user functions happens in subprocesses using the session interpreter (the REPL process and `eq.check`'s per-check process; the equation worker never evaluates repository code).
- Protocol changes bump `protocol_version` and are made on both sides in the same commit.
