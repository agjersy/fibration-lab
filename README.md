# Fibration Lab

**Fibration Lab** is the name of this suite: three workbenches that share one verification-first core.

| Workbench | What it is | Where |
|---|---|---|
| Analyst | Analysis tools an agent writes under your supervision, with code, math and figures made quick to verify | `crates/`, `python/kernel`, `apps/desktop` |
| Present | Decks built from the analysis, on your template, with every figure, equation and number traced to its run | `present/` |
| Math (working name Mathimator) | Live, replayable mathematical scenes whose plots and claims are backed by a symbolic engine | `mathimator/` |

Names in the code have not changed yet. The repository, the `analyst` and `mathimator` binaries, `~/.analyst`, and the crate and package names keep their current spelling until a separate, mechanical rename. The rest of this document describes the Analyst and Present workbenches under their current name, Personal Analyst; Math is described in [mathimator/README.md](mathimator/README.md).

## Personal Analyst

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
| `mathimator/` | Mathimator, the mathematics workbench that also ships as its own app: the SymPy engine, the scene SDK and cell runner, and the `math-core` / `mathimator` crates (see [mathimator/README.md](mathimator/README.md); product documents in `agjersy/mathimator`) |

One Cargo workspace and one pnpm workspace cover everything; `.github/workflows/ci.yml` runs both test suites on Linux and macOS, and the browser tests against the real core through `analyst-serve`. Windows is parked.

## Status

One app with two workbenches over one core: the **analysis workbench** (scripts, library, runs, figures, equations, packages, terminal) and the **deck workbench** (decks, slide rail, canvas, outline, references, sources, checks, notes, exports), docked in one window and driven by one agent chat in six modes (plan, build, run only, outline, slides, polish). What exists:

- Desktop app (Tauri 2; macOS and Linux targets): session tabs, CodeMirror editor with diagnostics and per-hunk proposal review, figures with run history and "Place on slide", equations panel, the deck panels beside the analysis panels, code diff cards and before/after slide cards in the chat, a slide's run citation opening its run in one click; a session reopens with its layout and the deck it had open
- `analyst` CLI: `init`, `index`, `check`, `run`, `eq`, `env`, `target`, `chat` (both tool sets), `eval` (analysis and deck tasks), `key`, and `deck init|list|template inspect|refs|render|check|build|eject|export|history|place-figure|place-equation`; the shell, `analyst-serve` and the CLI start through one function (`analyst_core::app::App::start`), and `analyst deck check` reads the project's run records, so it reports bound numbers as the desktop does
- Rust core: review-gated edits and scope declaration with three policy presets (`wb-policy`), OpenAI-compatible and Anthropic streaming adapters (`wb-providers`; native tool calling required), transcripts and compaction (`wb-transcript`), Agent Skills (`wb-skills`), kernel client over local stdio or SSH, repo sync and remote bootstrap, git checkpoints covering code and decks, decks under `decks/<slug>/` with a no-network deck kernel each, an artifact index whose run citations the deck checker reports as `stale` when the code changed, and one IPC command table from which the renderer's `commands.ts` is generated
- Python kernel: sandboxed detached runs with figure capture, script and library indexes, equation extraction (derived from code, or declared and numerically checked), the script contract checker, Ruff integration; the snippet REPL and the SymPy equation worker are two separate sandboxed processes, so snippet code cannot reach the equation checker
- Human verification: figures load through the asset protocol (new decks, also the agent's, are added to its scope at once); equations go from the analysis kernel to slides with their check result and export as native PowerPoint equations stored the way PowerPoint stores them (OMML in an `a14` choice, a picture fallback drawn by Matplotlib); scripts record named results that slide numbers, in text or table cells, bind to and the checker verifies; a verification ledger with a Verify queue; stale slide elements refresh from the analysis through a normal proposal, and a refresh that loses a race with an edit says so and offers to refresh again
- Evaluation suite: four analysis tasks and eight deck tasks, model-agnostic, with declarative graders and scripted model servers that drive the whole stack in CI (two analysis and two deck tasks)

**Verified on Linux** (this container): the Rust, Python and renderer test suites, the Playwright suite against the real core in a browser (through `analyst-serve`, not the Tauri window), the scripted agent loops through the `analyst` binary, the analysis kernel under a real `bubblewrap` sandbox, and deck exports opened by python-pptx and converted to PDF by LibreOffice.

**Not verified:** macOS (the `sandbox-exec` profile, FSEvents, the `.app` layout; CI runs the suites there but this was not checked here), real models (every agent loop here is driven by scripted model servers), PowerPoint itself (the exported files were checked against python-pptx and LibreOffice only), the real Tauri desktop window (the renderer was exercised in a browser), installers, and remote targets over SSH.

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) to build and run it.

**Getting started.** `tools/dev_setup.sh` checks the prerequisites on Linux or macOS (printing the install command for anything missing), sets up the Python environments and the renderer, builds the binaries, and prints how to start the desktop app and configure a model provider. `tools/make_sample_deck.sh` then builds `sample-deck/sample.pptx` (and a PDF with LibreOffice) through the real CLI and deck kernel, with no model: every element type on the corp test template, two native equations and a figure from a real run, plus a checklist of what to look at in PowerPoint.

## Documents

- [docs/VISION.md](docs/VISION.md): product vision, principles, core experiences, roadmap
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): application design and architecture
- [docs/PROTOCOL.md](docs/PROTOCOL.md): the contract between the kernel, the core, and the evaluation suite
- [docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md): decision record and the questions left open for later phases
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): layout, setup, running the CLI and the evaluation suite
