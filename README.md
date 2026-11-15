# Personal Present

A desktop workbench in which an AI agent builds presentations from your brief and reference materials, on your organization's template, under your supervision, and exports native, fully editable PowerPoint files (plus PDF, images, and outlines) whose every claim traces back to a source.

Model agnostic (Claude through the Anthropic API or an internal gateway; open-weight models on internal servers through OpenAI-compatible APIs), local first, enterprise deployable. Sister application to Personal Analyst, sharing its architecture and look.

## Status

**Ready to build.** The design is decided and the build specification is written. No code yet; implementation starts with [docs/BUILD-SPEC.md](docs/BUILD-SPEC.md).

## Documents

- [docs/VISION.md](docs/VISION.md): product vision, principles, core experiences, roadmap
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): application design and architecture
- [docs/PROTOCOL.md](docs/PROTOCOL.md): contract between the deck kernel, deck SDK, core, CLI, and evaluation suite
- [docs/IPC.md](docs/IPC.md): desktop contract between the React renderer and the Rust core
- [docs/BUILD-SPEC.md](docs/BUILD-SPEC.md): build order, work packages, and acceptance criteria for the coding agent
- [docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md): decision record and evidence to gather in phase 0
