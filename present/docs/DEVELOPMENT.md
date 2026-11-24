# Development

> **Historical design note.** This document describes Personal Present before it was folded into Personal Analyst. Where it conflicts with the documents in [`docs/`](../../docs/) (one core, one `analyst` CLI and desktop app, decks under `decks/<slug>/` in the project, git checkpoints instead of snapshots, the artifact index instead of the scan/import bridge, `~/.analyst` instead of `~/.present`), `docs/` wins. `PRESENT_HOME` and the `present` binary were removed.

Phase 0 is a headless build: the Rust core and CLI, the Python deck kernel and SDK, and the evaluation suite, with no desktop UI yet. See [ARCHITECTURE.md](ARCHITECTURE.md) for the design, [PROTOCOL.md](PROTOCOL.md) for the contracts, and [BUILD-SPEC.md](BUILD-SPEC.md) for the work packages.

## Layout

```
Cargo.toml             Rust workspace
crates/wb-providers/   shared-to-be: model adapters, capability probe (HTTP only through crates/wb-net)
crates/wb-policy/      shared-to-be: policy decisions, edit gates, scope, locks
crates/wb-transcript/  shared-to-be: transcript events and store, replay, compaction
crates/wb-skills/      shared-to-be: Agent Skills discovery and loading
crates/wb-sandbox/     shared-to-be: the present-sandbox helper (Windows AppContainer launcher)
crates/protocol/       deck, template, kernel, config, transcript, audit, and eval types; schema generation
crates/core/           projects, deck store, snapshots, index, workspace, agent loop, evaluation harness
crates/cli/            the `present` binary
python/sdk/            present_sdk: the deck SDK (templates, text fit, charts, tables, diagrams, tags)
python/kernel/         present_kernel: inspector, renderer, checker, fidelity engines, references, runner
schemas/               generated deck and slide JSON Schemas (committed)
evals/                 tasks with fixtures, test templates, the scripted model server
tools/                 template and fixture generators, CI checks, the CLI walk-through
vendor/fonts/          openly licensed fonts for deterministic text measurement in tests
```

## Prerequisites

- Rust stable (1.85 or newer)
- Python 3.12 or newer and `uv`
- Optional: LibreOffice (`soffice`) for fidelity renders, PDF and PNG export, and the overflow accuracy test. Tests that need it skip when it is missing.
- Optional: `bwrap` on Linux for the native sandbox. Without it, code runs with resource limits only and results say `sandbox: limits`.

## Setup and tests

```sh
# Python: the kernel environment also installs the SDK in editable mode
cd python/kernel
uv sync
uv run pytest -q                 # kernel and SDK behaviour (renderer, checker, references, code mode, diagrams)
uv run ruff check . ../sdk
cd ../..

# Rust (everything except the desktop shell, which needs a system WebView toolkit on Linux)
cargo build --workspace --exclude present-desktop
cargo test --workspace --exclude present-desktop    # core integration tests start the real kernel from python/kernel/.venv
cargo clippy --workspace --exclude present-desktop --all-targets -- -D warnings
cargo fmt --all -- --check

# The whole headless stack against a scripted model
tools/deck_walkthrough.sh   # now at the repository root, driving `analyst deck ...`
```

## Desktop shell

`apps/desktop/src-tauri` is the Tauri 2 shell: thin commands over `present_core::service`, one per row of [IPC.md](IPC.md) section 4. A test reads that table and fails when the shell and the document differ.

```sh
# Linux only: the WebView toolkit Tauri builds against
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev libsoup-3.0-dev

cargo test -p present-desktop
cargo build -p present-desktop
```

The service layer itself needs no window and is tested in `crates/core/tests/service.rs` against the real kernel with a scripted model. `PRESENT_ENGINE`, `PRESENT_SANDBOX`, and `PRESENT_PYTHON` overrode the rendering engine, the sandbox kind, and the kernel's interpreter (`PRESENT_HOME`, which overrode the home directory, was removed: the home is `ANALYST_HOME`).

Regenerating committed artifacts:

```sh
cd python/kernel
uv run python ../../tools/make_test_templates.py        # evals/templates
uv run python ../../evals/make_fixtures.py              # evals/tasks/*/fixture
uv run pytest -q --update-golden                        # golden slide XML, template descriptions, reference extracts
cd ../..
PRESENT_UPDATE_SCHEMAS=1 cargo test -p present-protocol --test schemas   # schemas/*.json
cargo test -p present-protocol --test ts_export                          # apps/desktop/src/ipc/generated
```

## Running the CLI in development

The CLI finds the kernel interpreter in this order: `PRESENT_RUNTIME_PYTHON`, `runtime_python` in `config.toml`, the bundled runtime under the Present home, then `python/kernel/.venv` of this checkout. After `uv sync`, development works with no configuration. The home directory was `%APPDATA%\Present` on Windows, else `~/.present` (`PRESENT_HOME` was removed; the home is now `~/.analyst` or `ANALYST_HOME`, and `~/.present` is migrated once).

```sh
cargo run -q -p present-cli -- init /tmp/demo --template evals/templates/corp/corp.potx
cd /tmp/demo
present refs add ~/reports/q3.pdf ~/data/pipeline.xlsx
present refs search "gross margin"
present chat -m <provider>/<model> --mode outline "A ten-slide quarterly review; lead with the margin recovery"
present chat -m <provider>/<model> "Build the slides from the approved outline"
present check
present export --format pptx        # also pdf, png, md, notes-docx
present history list
present eject                       # switch to code generation mode
present build
```

Providers are configured in `config.toml` under the home directory (PROTOCOL.md section 4.1). A provider's key is named by `key = "env:MY_KEY"` and read from the environment, then from `.env` in the home directory (the settings dialog writes it there). The app reaches the network only for model endpoints, package installs, and SSH targets, through the network gate in `crates/wb-net`, which logs each decision to `egress.jsonl` in the home directory; there is no proxy, CA bundle, or keychain support. The deck kernel is started with no network (`bwrap` on Linux, `sandbox-exec` on macOS).

## Evaluation suite

Each task under `evals/tasks/<name>/` has a `task.toml` (prompt, mode, checks) and a `fixture/` with a `TEMPLATE` file naming a template under `evals/templates`, `sources/` to add as references, and optionally a starting `deck/`, `assets/`, and `project.toml`. Checks look only at the finished project.

```sh
present eval run --model <provider>/<model>                       # all tasks
present eval run --model <provider>/<model> --task report-to-deck --work /tmp/eval-work
```

### Exercising the loop without a model

`evals/fake_model_server.py` is a scripted OpenAI-compatible server (stdlib only). It answers the capability probe and plays `spreadsheet-to-charts` and `fix-checker-failures`, declining everything else. It drives the real provider adapter, agent loop, policy, tools, kernel, checker, and graders, and runs in CI on all three operating systems.

```sh
python3 evals/fake_model_server.py --port 8765 &
mkdir -p /tmp/present-home && cat > /tmp/present-home/config.toml <<'CONF'
[defaults]
model = "fake/scripted"
[[providers]]
name = "fake"
kind = "openai-compatible"
base_url = "http://127.0.0.1:8765/v1"
auth = "none"
CONF
ANALYST_HOME=/tmp/analyst-home analyst eval run --tasks evals/tasks --task spreadsheet-to-charts   # was: present eval run (PRESENT_HOME removed)
```

The other six tasks need a real model and fail by design against the scripted server.

## Conventions

- Rust: `cargo fmt`, clippy clean, no `unwrap()` on I/O paths in library code.
- Python: Ruff clean. The kernel and SDK use only the fixed package set (PROTOCOL.md 1.3); test-only dependencies go in the `dev` group.
- The kernel never executes deck scripts, generated slides, or skill scripts in its own process: they run in the sandboxed runner.
- Contract changes update PROTOCOL.md, both sides, generated schemas, and tests in the same commit.
- Every new dependency is added to `THIRD-PARTY.md` with its license in the same commit.
