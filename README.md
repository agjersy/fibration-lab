# Personal Analyst

A desktop workbench for scientific, technical, and numerical analysis: an AI agent writes reusable Python analysis tools under your supervision, and the application makes verifying that work (code, math, figures) fast.

Model agnostic (open-weight models via Ollama or any OpenAI-compatible server, frontier models via API keys), local first, sandboxed execution, with runs on this machine or on a remote Linux host over SSH.

## One product

Personal Present (the deck workbench) has been folded in: Personal Analyst is one desktop app and one `analyst` CLI over one core, with one agent loop, one policy engine, one transcript, and one undo timeline. A project is one git repository; its decks live in it under `decks/<slug>/` and are checkpointed by git with the code. Slides cite analysis runs through the artifact index. The home is `~/.analyst` (`ANALYST_HOME`); an old `~/.present` is migrated once on first start and left in place.

| Path | Contents |
|---|---|
| `crates/wb-*` | Shared core modules: `wb-net` (the network gate, egress ledger, and `.env` secrets), providers, policy, transcript, skills, sandbox launcher |
| `crates/protocol`, `crates/core`, `crates/cli`, `apps/desktop` | The one core (with the one IPC command table), the `analyst` CLI, and the one desktop shell and renderer |
| `crates/evals` | `analyst-evals`: the analysis and deck evaluation harnesses, used by `analyst eval` |
| `crates/serve` | `analyst-serve`: development bridge that serves the real core to the renderer in a browser over a loopback WebSocket; never shipped |
| `present/crates/core`, `present/crates/protocol` | `analyst-deck` and `analyst-deck-protocol`: the deck library of the core (deck store, references, deck kernel client, deck tools, per-deck service) |
| `python/kernel`, `present/python/kernel`, `present/python/sdk` | The analysis kernel, the deck kernel, and the deck build SDK |
| `evals/`, `present/evals/` | Analysis and deck evaluation tasks, both run by `analyst eval run` |
| `packages/wb-ui` | Shared UI package |

One Cargo workspace and one pnpm workspace cover everything; `.github/workflows/ci.yml` runs both test suites on Linux and macOS, and the browser tests against the real core through `analyst-serve`. Windows is parked.

## Status

**Phase 0 (headless core) and phase 1 (desktop workbench) are built.** What exists:

- Desktop app (Tauri 2, macOS and Linux), one window and one renderer for both workbenches: session tabs, project and scripts sidebar with run forms, library and runs views, CodeMirror editor with diagnostics and per-hunk proposal review, figures panel with run history and comparison (and "Place on slide"), equations panel, packages panel, terminal, run output and diagnostics docks; the deck panels (Decks view with a New deck dialog, slide rail, canvas with in-place editing, outline, references, sources, checks, notes, exports) in the same dock, so panels of both workbenches can sit side by side; one agent chat in all six modes with code diff cards and before/after slide cards; a slide figure's run citation opens its run in one click; Zed keymap subset and command palette

- `analyst` CLI: `init`, `index`, `check`, `run`, `eq`, `env`, `target`, `chat` (both tool sets), `eval` (analysis and deck tasks), `key`, and `deck init|list|template inspect|refs|render|check|build|eject|export|history`
- Rust core: review-gated edits and scope declaration with three policy presets (shared `wb-policy`), OpenAI-compatible and Anthropic streaming adapters with capability probing (shared `wb-providers`; native tool calling required), transcripts and compaction (shared `wb-transcript`), Agent Skills (shared `wb-skills`), kernel client over local stdio or SSH, repo sync and remote bootstrap, git checkpoints covering code and decks, one agent loop with the analysis and deck tool sets and modes (plan, build, run, outline, slides, polish) as policy profiles, decks under `decks/<slug>/` with a no-network deck kernel each, and an artifact index whose run citations the deck checker reports as `stale` when the code changed
- Python kernel: sandboxed detached runs with figure capture, script and library indexes, equation extraction (derived from code, or declared and numerically checked), the script contract checker with duplicate-logic detection, Ruff integration; the snippet REPL and the SymPy equation worker are two separate sandboxed processes per session, so snippet code cannot reach the equation checker
- Human verification: figures load through the asset protocol; equations go from the analysis kernel to slides with their check result and export as native PowerPoint equations (OMML); scripts record named results that slide numbers bind to and the checker verifies; a per-project verification ledger (`.analyst/verification.jsonl`) with a Verify queue, badges, and an export warning for unverified dependencies; stale slide elements refresh from the analysis through a normal proposal
- Evaluation suite: four analysis tasks and eight deck tasks, model-agnostic, with declarative graders and scripted model servers that drive the whole stack in CI (two analysis and two deck tasks)

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) to build and run it.

## Documents

- [docs/VISION.md](docs/VISION.md): product vision, principles, core experiences, roadmap
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): application design and architecture
- [docs/PROTOCOL.md](docs/PROTOCOL.md): the contract between the kernel, the core, and the evaluation suite
- [docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md): decision record and the questions left open for later phases
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): layout, setup, running the CLI and the evaluation suite
