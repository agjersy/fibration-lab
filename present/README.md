# Personal Present

> **Folded into Personal Analyst.** This folder now holds the deck library of the one product (`analyst-deck`, `analyst-deck-protocol`), the deck kernel and SDK, deck evaluations, and the old deck renderer as porting material. The `present` CLI and desktop shell are gone: use `analyst deck ...`, `analyst chat`, and `analyst eval run` (see the root [README](../README.md) and [docs/DEVELOPMENT.md](../docs/DEVELOPMENT.md)). The description below is historical.

A desktop workbench in which an AI agent builds presentations from your brief and reference materials, on your organization's template, under your supervision, and exports native, fully editable PowerPoint files (plus PDF, images, and outlines) whose every claim traces back to a source.

Model agnostic (Claude through the Anthropic API or an internal gateway; open-weight models on internal servers through OpenAI-compatible APIs), local first, enterprise deployable. Sister application to Personal Analyst, sharing its architecture and look.

## Status

**Phase 0 (headless core) is built and passes its tests on Linux; phase 1 (desktop app) is not started.** What exists:

- `present` CLI: `init`, `template inspect`, `refs`, `render`, `check`, `build`, `eject`, `export`, `history`, `chat`, `models probe`, `eval run`
- Rust core: projects, deck store with conflict detection, append-only snapshots, reference store, hybrid search index, audit log, kernel client, the agent loop (outline, build, polish) with proposals gated by policy, and the evaluation harness
- Shared crates written for both this app and Personal Analyst: model providers with gateway support, policy, transcript, skills, and the Windows sandbox launcher
- Python deck SDK and kernel: template inspection, deterministic native `.pptx` rendering (text, images, charts, tables, four diagram families), the deck checker with source and number checks, PowerPoint and LibreOffice fidelity rendering, reference ingestion for eleven formats, declarative chart transforms, code generation mode with sandboxed builds and a verified eject
- Evaluation suite: eight tasks with graders and a scripted model server that drives the whole stack in CI

Not yet verified: anything on Windows or with PowerPoint itself, the macOS sandbox, a real embedding model, and runs against real models. See [docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md) section 4.

## Documents

- [docs/VISION.md](docs/VISION.md): product vision, principles, core experiences, roadmap
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): application design and architecture
- [docs/PROTOCOL.md](docs/PROTOCOL.md): contract between the deck kernel, deck SDK, core, CLI, and evaluation suite
- [docs/IPC.md](docs/IPC.md): desktop contract between the React renderer and the Rust core
- [docs/BUILD-SPEC.md](docs/BUILD-SPEC.md): build order, work packages, and acceptance criteria for the coding agent
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): layout, setup, running the CLI and the evaluation suite
- [docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md): decision record and evidence to gather in phase 0
