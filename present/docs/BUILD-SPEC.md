# Build Specification

> **Historical design note.** This document describes Personal Present before it was folded into Fibration Lab. Where it conflicts with the documents in [`docs/`](../../docs/) (one core, one `fibration` CLI and desktop app, decks under `decks/<slug>/` in the project, git checkpoints instead of snapshots, the artifact index instead of the scan/import bridge, `~/.fibration` instead of `~/.present`), `docs/` wins. `PRESENT_HOME` and the `present` binary were removed.

*Status: ready to build (2026-11-15). Audience: the coding agent (and any human) implementing Personal Present. This document says what to build, in what order, and how each step is accepted. Read in this order: [VISION.md](VISION.md) (why), [OPEN-QUESTIONS.md](OPEN-QUESTIONS.md) section 2 (the decision record), [ARCHITECTURE.md](ARCHITECTURE.md) (design), [PROTOCOL.md](PROTOCOL.md) and [IPC.md](IPC.md) (normative contracts), then this. When this document and a contract disagree, the contract wins; fix this document in the same change.*

## 1. Ground rules

1. **The reference implementation is Fibration Lab** (`agjersy/fibration-lab`, branch `claude/analyst-desktop-app-spec-ef5mu2`, commit `f229e4d` or later). Mirror its structure, conventions, CI, and test style. Where a work package says "port", copy the code with a header comment naming the source path and commit, then adapt.
2. **Do not modify the Analyst repository** from this work. Extraction into a shared repository (section 4) is a separate, explicitly approved task.
3. **No model-specific code paths.** No prompt text, tool schema, or branch conditioned on a model or provider name. Capability records only.
4. **Permissive licenses only.** No GPL, AGPL, or LGPL dependencies (Rust, Python, npm, bundled binaries, model weights). MPL-2.0 only for LibreOffice as an unmodified external program. Every new dependency is added to `THIRD-PARTY.md` with its license in the same commit. Known exclusions: PyMuPDF (AGPL), `extract-msg` (GPL), `python-pptx` forks under other licenses.
5. **Offline by construction.** Nothing in an installed build downloads anything at run time: runtime, wheels, embedding model, and fonts used by tests are bundled or vendored. Development and CI may download.
6. **Determinism.** `deck.render` is byte-reproducible (PROTOCOL 1.4); tests assert it.
7. **Contracts move together.** A change to a JSON-RPC method, the deck schema, an IPC command, or a transcript event updates PROTOCOL.md or IPC.md, both sides, generated types, and tests in one commit. Bump `protocol_version` or `schema_version` on breaking changes.
8. **Every work package ships its tests** and keeps CI green on Windows and macOS (Linux for headless jobs). A package is done only when its acceptance criteria pass in CI.
9. **Conventions** (from Analyst): `cargo fmt`, clippy clean with `-D warnings`, no `unwrap()` on I/O in library code; Ruff clean; TypeScript strict, ESLint clean; commit messages in the imperative, one logical change each.
10. **Ask instead of guessing** when a contract is ambiguous: record the question in a new section 4, "Questions from the build", of `docs/OPEN-QUESTIONS.md` and take the most conservative reading meanwhile.

## 2. Scope

| Phase | In scope | Exit criterion |
|---|---|---|
| 0. Headless core | Everything in section 5: protocol, providers, kernel, SDK, renderer, diagrams, checker, fidelity render, references and search, project and snapshot store, sandbox, code mode, agent loop, CLI, evaluation suite | Section 5.16 |
| 1. Workbench | Everything in section 6: Tauri app, renderer UI, live preview, proposals, source lens, exports, settings, installers for Windows and macOS | Section 6.12 |

Out of scope until later phases (do not build, but do not design them out): re-import of PowerPoint edits (stable ids are in scope), import of arbitrary decks as projects, managed configuration and organization libraries, Bedrock/Vertex/Foundry auth back ends, OCR, web access, `.odp`/HTML/`.potx` export, Linux packaging, animations.

## 3. Repository layout and toolchain

```
Cargo.toml                 Rust workspace
crates/wb-providers/       shared-to-be: model adapters, probe, network (section 4)
crates/wb-policy/          shared-to-be: policy evaluation and gating
crates/wb-transcript/      shared-to-be: transcript store, compaction
crates/wb-skills/          shared-to-be: Agent Skills loader
crates/wb-sandbox/         shared-to-be: sandbox wrappers (macOS profile, Windows AppContainer, Linux bwrap)
crates/protocol/           Present domain types, kernel RPC types, IPC types; schemars + ts-rs export
crates/core/               projects, deck store, snapshots, index, agent loop, tools, kernel client, audit
crates/cli/                `present` binary
apps/desktop/              phase 1 (src-tauri + src)
packages/wb-ui/            shared-to-be: theme, chat components, Backend interface (phase 1)
python/kernel/             present_kernel
python/sdk/                present_sdk
schemas/                   generated deck.schema.json, slide.schema.json (committed)
evals/                     tasks, templates, fake_model_server.py, results/ (ignored)
tools/                     make_test_templates.py, bundle_runtime.py, fetch_embedding_model.py, fetch_ooxml_schemas.py
vendor/fonts/              OFL test fonts (Noto Sans, Noto Serif, Liberation Sans) for deterministic measurement tests
docs/
THIRD-PARTY.md
```

Toolchain: Rust stable (1.85 or newer); Python 3.12 managed by `uv` for development (`python/kernel` and `python/sdk` are `uv` projects; the kernel depends on the SDK by path); Node 22 and pnpm 10 (phase 1); Tauri 2. Pin exact dependency versions in lockfiles.

Python fixed package set (PROTOCOL 1.3 `kernel.hello.packages`), pinned in `python/kernel/pyproject.toml` and frozen into the bundled runtime: `python-pptx`, `lxml`, `Pillow`, `openpyxl`, `python-docx`, `pypdfium2`, `numpy`, `pandas`, `matplotlib`, `jsonschema`, `uharfbuzz`, `olefile`, `beautifulsoup4` (with the stdlib `html.parser`), `onnxruntime`, `tokenizers`, `zstandard`, plus `pywin32` on Windows. The sandboxed code sees the same interpreter and set.

## 4. Shared code strategy

Decided (OQ-P11): a third repository will hold code shared with Analyst. That repository does not exist yet and creating it needs the owner. Therefore:

- **Now:** the shared-to-be crates and `packages/wb-ui` live in this workspace under `wb-*` names, with no dependency on Present-specific crates (enforced by a CI check that `crates/wb-*` depend only on each other and third-party crates). Their APIs are written for both apps.
- **Port sources** (from Analyst at the commit in rule 1): `crates/core/src/providers/*` → `wb-providers`; `crates/core/src/policy.rs` → `wb-policy`; transcript and compaction pieces of `crates/core/src/agent/*` and `crates/protocol/src/transcript.rs` → `wb-transcript`; `python/kernel/fibration_kernel/sandbox.py` profile logic → `wb-sandbox` (reimplemented in Rust, same generated profile); `apps/desktop/src/styles/theme.css`, `ui/chat/*`, `ui/common/*`, `ipc/backend.ts` → `packages/wb-ui`.
- **Since the merge into one repository:** the analysis core uses `wb-providers`, `wb-policy`, `wb-transcript` and `wb-skills` too, and its own copies are deleted. The fixes the analysis copies had received after the port commit were carried into the shared crates first. `wb-policy/src/regions.rs` (comment-marker locked regions) and the text protocol are removed from both.
- **Later (separate approved task):** move `wb-*` and `packages/wb-ui` into the shared repository with history, switch Present to tag-pinned git dependencies, then switch Analyst in its own change.

## 5. Phase 0 work packages

Order matters: each package lists what it depends on. Packages without dependencies between them may proceed in parallel. Spike S1 runs first because it carries the most platform risk.

### S1. Sandbox spike (Windows AppContainer, macOS sandbox-exec)

**Do:** in `crates/wb-sandbox`, a minimal `run_sandboxed(spec) -> Output` that launches a process with read-only access to listed directories, write access to a scratch directory, no network, and job-object limits on Windows; macOS via a generated `sandbox-exec` profile ported from Analyst.
**Accept:** integration tests on Windows and macOS CI runners that, from inside the sandbox, (a) reading an allowed file succeeds, (b) reading a file in the user's home outside the allowed set fails, (c) writing outside scratch fails, (d) an outbound TCP connection to `1.1.1.1:443` fails, (e) a memory hog is killed at the cap, (f) the runtime's Python starts and imports `pptx`, `pandas`, `matplotlib`. Write `docs/notes/sandbox-spike.md` with findings (ACL handling, profile cleanup, latency per launch).

### W0. Scaffold and CI

**Depends:** none.
**Do:** workspace, crate and package skeletons, `README` dev section, `docs/DEVELOPMENT.md` (mirroring Analyst's), `THIRD-PARTY.md`, GitHub Actions: Rust fmt/clippy/test on `windows-latest`, `macos-latest`, `ubuntu-latest`; Python Ruff and pytest on the same three; license check job (`cargo-deny` for Rust with a deny list for GPL/AGPL/LGPL; `pip-licenses` for the Python set; `license-checker` for npm in phase 1); the `wb-*` dependency-direction check.
**Accept:** CI green on an empty-but-compiling workspace; `cargo deny check licenses` passes.

### W1. Protocol types and schemas

**Depends:** W0.
**Do:** `crates/protocol`: Rust types for the deck document (PROTOCOL 2), template description (3), config files (4), kernel RPC params and results (1.3), diagnostics (1.5), transcript additions (6.2), audit record (10), eval task format (12). `schemars` generates `schemas/*.schema.json`; `ts-rs` generates TypeScript (phase 1 consumer). A `cite` parser and formatter implementing the grammar in PROTOCOL 2.3.
**Accept:** round-trip tests (serialize, parse, compare) for every type with hand-written fixtures in `crates/protocol/tests/fixtures/`; a test that regenerates the schemas and fails if the committed files differ; cite grammar tests including every row of PROTOCOL 2.3 and malformed inputs.

### W2. Providers with gateway support (`wb-providers`)

**Depends:** W0.
**Do:** port Analyst's provider layer (both adapters, SSE, `ModelEvent`, scripted client, capability probe; the text protocol was ported and later removed, native tool calling is required). Add: `auth = bearer|x-api-key|none`, extra headers, `location`, unknown-field downgrade (PROTOCOL 6.1), document blocks with citation mapping, prompt-caching markers, the vision probe, `degraded` computation against the 128k + vision baseline, keys named by `env:<VAR>` and read from the environment or `.env`, the audit hook (a callback invoked per request with the PROTOCOL 10 record minus content).
**Accept:** the Analyst SSE fixture tests pass unchanged; new fixtures for bearer auth, extra headers, a 400 naming `cache_control` followed by a successful retry, an Anthropic stream with `citations` deltas mapped to `Cite`, the vision probe pass and fail; a request to an endpoint that is not allowed at the network gate is refused before any connection. (Proxy, CA bundle, and keychain support were removed: network only for model endpoints, package installs, and SSH targets, through the network gate (`wb-net`); keys from the environment or `.env`.)

### W3. Kernel skeleton and runtime bundling

**Depends:** W0, W1.
**Do:** `present_kernel` with framing, dispatch, errors, `kernel.hello`/`shutdown`, `task.progress`, logging to stderr (port Analyst's `rpc.py`). Core-side `KernelClient` (port Analyst's `kernel/client.rs` and `transport.rs`, local stdio only). `tools/bundle_runtime.py`: builds a relocatable runtime from `python-build-standalone` plus an offline wheelhouse of the fixed set, for the current OS and architecture (macOS arm64 and x86_64, Windows x86_64 and arm64).
**Accept:** core spawns the kernel and completes hello/shutdown on all three OSes; `bundle_runtime.py` output runs `-m present_kernel --version` on a CI runner with network disabled for that step (`unshare -n` on Linux; on Windows and macOS a firewall-free check that `pip` is absent and no network is attempted, by setting an invalid proxy).

### W4. Test templates and template inspector

**Depends:** W3.
**Do:** `tools/make_test_templates.py` generating three templates into `evals/templates/` from python-pptx's default template by XML edits: `plain` (default), `corp` (custom theme colors and fonts using the vendored Noto fonts, 10 layouts including a "Big Number" layout, renamed and unhelpfully named layouts like "Layout 7", a footer placeholder, a Disclaimer layout), `wide-dense` (16:9, small placeholders, Liberation Sans) and sidecars for `corp`. `template.inspect` and `template.sidecar` per PROTOCOL 3 and 6.4; the compact catalog rendering used by `template.layouts`.
**Accept:** inspector output for each test template matches committed golden JSON; placeholder keys are unique and stable across runs; `fonts_missing` reports a font removed from the font path in a test; real templates the owner supplies privately (not committed) can be inspected via `present template inspect <path>` and the results recorded in `docs/notes/template-coverage.md` (evidence item in OPEN-QUESTIONS section 3).

### W5. Deck SDK and structured renderer

**Depends:** W1, W4.
**Do:** `present_sdk` public API (PROTOCOL 5.1) for slides, placeholders, text with levels and run formatting, theme colors, images with fit and crop, native charts (all types in 2.4 with embedded workbook and number formats), native tables (template table style, header and banding), notes, sections, footer and classification, document properties, stable ids and tags (2.7), citations as tags, alt text. `deck.render` (PROTOCOL 1.3) builds the deck from the document through the SDK. `deck.validate`.
**Accept:** for each test template, a corpus of at least 30 fixture slides (every placeholder content type, every chart type, multi-level text, links, theme colors, locked slides) renders; output re-opens in `python-pptx` and every element is found by its tag; render twice gives byte-identical files; a golden test compares slide XML (normalized) for 10 fixtures; charts open their embedded workbook with `openpyxl` and contain the computed values.

### W6. Diagram primitives

**Depends:** W5.
**Do:** `present_sdk.diagrams` per PROTOCOL 2.5 and 5.3: process (chevrons, steps, flowchart with layered layout and orthogonal elbow connectors), timeline (milestones, swimlanes, Gantt), matrix (2x2, grid with Harvey balls drawn as pie shapes, KPI tiles), hierarchy (org, tree, driver tree), each as a named group of native shapes and connectors with theme styling, text auto-sized with `text.measure`.
**Accept:** fixtures for every variant at 3, 7, and 15 items render without overlapping shapes (geometric test on bounding boxes) and without text overflow at the corp template's body size, or report `fit.overflow` when the content is deliberately too long; fidelity renders of the fixtures are attached to the CI run as artifacts for visual review.

### W7. Checker and text measurement

**Depends:** W5.
**Do:** `text.measure` with `uharfbuzz` shaping, line breaking on spaces and CJK boundaries, line spacing and paragraph spacing per placeholder level, autofit shrink search (PowerPoint's shrink steps: font scale in 2.5 % decrements down to `min_font_pt`, line spacing reduction to 80 %), font resolution through the OS font directories plus `template/fonts/` and `vendor/fonts/` in tests. All rules in PROTOCOL 7 except `src.numbers` and `src.chart-stale` (W12) and `out.opens-clean`'s fidelity part (W8). `tools/fetch_ooxml_schemas.py` fetches the ECMA-376 transitional XSDs in development and CI (not committed until their redistribution terms are confirmed) and validation runs with `lxml`.
**Accept:** measurement fixtures: 40 text boxes rendered by LibreOffice in CI, with the checker's fits/overflow verdict agreeing on at least 38 (record misses in `docs/notes/overflow-accuracy.md`); each rule has a passing and a failing fixture; contrast computed against placeholder fill, layout background, and master background in that order.

### W8. Fidelity renderer

**Depends:** W5.
**Do:** engine discovery (PROTOCOL 1.3 `EngineInfo`): PowerPoint on Windows via COM (`pywin32`: open read-only, `SaveAs` PDF or per-slide PNG export, quit, with a watchdog that kills `POWERPNT.EXE` it started after a timeout), PowerPoint on macOS via `osascript` (export as PDF and PNG; detect the Automation permission prompt and report it as `EngineInfo.detail`), LibreOffice via `soffice --headless --convert-to pdf` with a private `-env:UserInstallation` profile per kernel, PNG via `pypdfium2` from the PDF. A single queue per engine. `render.fidelity`, `template.thumbnails`, and the fidelity part of `out.opens-clean`.
**Accept:** LibreOffice path tested on all three CI OSes (installed in CI); PowerPoint paths tested by a workflow that runs only on a self-hosted runner labeled `powerpoint` (optional; skipped otherwise) and by unit tests with the COM/osascript layer mocked; render of a 30-slide fixture deck under 30 s with LibreOffice on CI hardware (record the number).

### W9. Reference ingestion and embeddings

**Depends:** W3.
**Do:** extractors for every `RefKind` (ARCHITECTURE 8, PROTOCOL 1.3): PDF (`pypdfium2`, per-page text with character offsets, embedded images, tables via a simple ruling-line and whitespace-column heuristic, pages without text flagged), DOCX (paragraphs with heading paths, tables), PPTX (per slide and shape, notes, chart data), XLSX (sheets, named ranges, used ranges as tables, evaluated values), CSV (dialect sniffing), Markdown and text, images, EML (stdlib `email`, attachments as children), MSG (an `olefile`-based reader per MS-OXMSG: subject, sender, recipients, date, plain or RTF-decompressed body, attachments), HTML and MHTML (BeautifulSoup main-content heuristic: drop nav/aside/footer/script/style, keep headings and tables). Chunker per PROTOCOL 8.1. `embed.texts` with the bundled model: select a multilingual sentence-embedding model under a permissive license with ONNX weights of at most 300 MB (candidates to evaluate: `multilingual-e5-small`, `bge-small` family, `gte` small models; record the choice, license, and evaluation in `docs/notes/embedding-model.md`); `tools/fetch_embedding_model.py` downloads and checksums it for development and bundling.
**Accept:** fixture files for every kind in `python/kernel/tests/fixtures/refs/` (created by scripts in the repo, not copied from the internet) extract to golden outlines and chunks; locations in chunks round-trip through `refs.read`; an MSG fixture (generated with a small writer in the test helpers) with an attachment yields a child reference; embedding of 1000 chunks completes in under 60 s on CI CPU.

### W10. Project store, snapshots, index, audit

**Depends:** W1, W9.
**Do:** in `crates/core`: project create/open (layout in ARCHITECTURE 15, files in PROTOCOL 4), deck store with per-slide content hashes and conflict detection, reference copy-in with content addressing and versioning (`manifest.toml`), snapshot store (PROTOCOL 4.4) with zstd, restore, revert-turn, pruning; SQLite index with FTS5 and `sqlite-vec` (PROTOCOL 8.1), hybrid search (8.2), incremental background ingest; audit log writer and CSV export.
**Accept:** property tests: random sequences of slide edits, snapshots, restores, and prunes never lose a `mode-switch` or `export` snapshot and restore reproduces exact bytes; adding the same file twice stores it once; a changed file creates a new version and old citations still resolve; hybrid search fixture queries return the expected chunk in the top 3 (fixture set with 10 queries); the audit log never contains message text (test greps for a sentinel string placed in a prompt).

### W11. Code mode, generated slides, eject

**Depends:** S1, W5, W10.
**Do:** `present_kernel.runner` and `deck.build`, `slide.generate`, `skill` script execution through `wb-sandbox` (PROTOCOL 9); element-map completion for raw shapes; `deck.eject` with verification (PROTOCOL 5.4); `code.hardcoded-style` lint; switching logic in the core (to code: eject, verify, snapshot, flip mode; back: revert to the pre-eject snapshot, or the import path marked unsupported in phase 0 with a clear error).
**Accept:** ejecting every structured fixture deck verifies; a code-mode fixture using raw `python-pptx` for a custom shape builds and passes `out.opens-clean`; a script that tries to open a socket, read `~/.ssh`, or write outside scratch fails with the run reported as failed and nothing written to `deck/`; a script exceeding the timeout is killed; `sandbox: "limits"` fallback is badged in results.

### W12. Agent loop, tools, policy, skills

**Depends:** W2, W7, W10, W11.
**Do:** port Analyst's loop structure (turn algorithm, pause-on-ask, compaction with pinned facts, max steps, summaries) into `crates/core` using `wb-policy`, `wb-transcript`, `wb-skills`. Implement the tool set (PROTOCOL 6.3), modes (outline, build, polish with polish's edit restrictions enforced by the policy layer, not the prompt), context assembly (ARCHITECTURE 7.1) with the compact layout catalog and reference outlines, scope declaration, the post-edit verification loop (check, at most 3 automatic fix iterations), the self-review requirement (`slide.render` for every new or changed slide before the turn summary; enforced by a turn-end check that re-prompts once), the number check (PROTOCOL 8.3) and `src.numbers`, chart compute and `src.chart-stale`, document blocks for small PDF sets when the capability allows. System prompt in `crates/core/src/agent/prompt.md` (single file, no per-model variants), covering action titles, one message per slide, citing every claim, using layouts and primitives instead of free elements, and the deck contract.
**Accept:** scripted-model tests (Analyst's `ScriptedModelClient`) for: a review-mode turn pausing and resuming on accept, reject, and accept-modified; an out-of-scope slide edit in auto mode pausing; an edit to a locked slide denied; polish mode rejecting a new slide and a new citation; the verification loop fixing an overflow; skills listed by description and loaded on demand; a skill script running sandboxed with `ask`; the number check on 20 fixture runs (green, amber, red, grey cases) matching expectations.

### W13. CLI

**Depends:** W12.
**Do:** `present` with subcommands: `init <dir> --template <file>`, `template inspect <file>`, `refs add <files...>`, `refs search <query>`, `render [--out]`, `check`, `build` (code mode), `eject`, `export --format pptx|pdf|png|md|notes-docx [--preset]`, `history [list|restore|revert-turn]`, `chat [-m model] [--mode] "<text>"` (with `--auto` approving per policy for evals), `models probe`, `eval run`.
**Accept:** a shell-script integration test in CI walks through init, refs add, a scripted chat that creates three slides, check, export to pptx/pdf/md/notes-docx, history revert-turn, and asserts the outputs.

### W14. Evaluation suite

**Depends:** W13.
**Do:** harness (port Analyst's `evals.rs` and scorecard), check kinds (PROTOCOL 12), `fake_model_server.py` plays for at least two tasks, and these tasks, each with fixtures generated by scripts in `evals/`:

| task | prompt gist | key checks |
|---|---|---|
| `report-to-deck` | 8 to 10 slides from a 30-page synthetic annual report PDF on the `corp` template | opens_clean, checker_clean, slide_count, citation_coverage >= 0.9, numbers_verified >= 0.9, uses_native chart >= 1 |
| `spreadsheet-to-charts` | 5 slides charting a synthetic sales workbook, including a top-5 with "Other" and a percent change | uses_native chart >= 4, numbers_verified, no free elements |
| `email-thread-brief` | 3-slide status brief from an EML thread with a PPTX attachment | citation_coverage, export_exists md |
| `restyle-template` | move an existing structured deck from `plain` to `corp` | layout_used, checker_clean, locked_untouched |
| `shorten-deck` | cut a 15-slide deck to 6 without losing the cited numbers | slide_count exact, numbers_verified, edits_within_scope |
| `fix-checker-failures` | a deck with overflow, off-theme colors, missing alt text | checker_clean |
| `roadmap-diagram` | a timeline and a process slide from a plan document | uses_native diagram >= 2, checker_clean |
| `code-mode-custom` | switch to code mode and build a custom radial diagram slide | mode code, opens_clean, checker_clean (with code.hardcoded-style <= 0 warnings) |

**Accept:** with the fake model server, the scripted tasks PASS in CI on all three OSes; with a real model configured via environment (owner-provided endpoint), `present eval run` produces a scorecard (not part of CI).

### 5.16 Phase 0 exit

All packages accepted; then, run by the owner with real endpoints: three tasks complete end to end with two models from different providers (Claude through the API or gateway and one baseline-meeting open-weight model) on two templates, every export opens without a repair prompt in PowerPoint on Windows and macOS (manual check recorded in `docs/notes/phase0-exit.md`), and the evidence notes from OPEN-QUESTIONS section 3 exist.

## 6. Phase 1 work packages

### D1. Desktop scaffold and shared UI package

**Do:** `apps/desktop` (Tauri 2, React 19, Vite, Zustand, dockview, CodeMirror 6) with the same structure as Analyst's (`ipc/`, `state/`, `ui/`); `packages/wb-ui` ported from Analyst (theme tokens unchanged, chat panel, cards, dialogs, toasts, palette, `Backend` interface, mock helpers); `src-tauri` commands as thin glue over `present_core::service`.
**Accept:** `pnpm dev` runs against MockBackend; `pnpm typecheck lint test build` green; `cargo build -p present-desktop` on Windows and macOS.

### D2. MockBackend

**Do:** per IPC.md section 6, with pre-rendered fixture PNGs.
**Accept:** unit tests for every mock command and the chat scenarios; Playwright e2e uses it.

### D3. Service layer and IPC

**Do:** `present_core::service` (per-project runtime, event channel, task registry), all commands and events in IPC.md, generated TypeScript types with a compatibility test (as Analyst's `types.compat.test.ts`).
**Accept:** Rust integration tests drive every command against a temp project with the real kernel.

### D4. Workbench layout and project tabs

**Do:** top bar with project tabs (template name, model, kernel state), model picker with degraded badges, settings dialog (providers, keys into the home `.env`, render engine, embedding, kernel network isolation), new-project dialog (folder, template file, deck mode), layout persistence, Zed-style palette and keymap subset from Analyst.
**Accept:** e2e: create a project from the mock, switch tabs, layout restored after reload.

### D5. Slide rail, outline view, canvas with live preview

**Do:** thumbnail rail with sections, drag reorder, badges (checks, numbers, lock, generated, code mode); outline table editor; the live preview renderer (`ui/slide/`): placeholder geometry from the template description, text with level styles and theme fonts, autofit approximation, charts as SVG from `computed` data, tables, diagram primitives drawn from the same layout rules (the kernel exposes computed diagram geometry through `deck_get` so the renderer does not reimplement layout), images with crop; in-place text editing (contentEditable restricted to the run model), layout picker with thumbnails, lock toggles, chart/table/diagram data side sheets; code mode shows the canvas read-only with the fidelity thumbnail.
**Accept:** visual regression tests (Playwright screenshots in mock mode) for the eight mock slides; editing a bullet issues `slide_update` with a correct document; preview geometry within 2 px (at 1280 px width) of the fidelity render for placeholder boxes on the corp template (computed test, not visual).

### D6. Chat, cards, and proposals

**Do:** chat with streaming, mode switch, context indicator, attachments (slides, cites, images); cards: outline (editable table), slide proposal (before/after images, text diff, accept, reject, accept-with-edits opening the canvas), code proposal (CodeMirror merge view per hunk), build output, skill script approval, mode switch, export; turn summary card.
**Accept:** e2e for each mock scenario in IPC.md section 6.

### D7. References panel and source lens

**Do:** references list with status and sizes, add via dialog and drag-and-drop, remove unused; Analyst import dialog (scan a repo, pick items); Sources panel showing `SourcePassage` (text with highlight, PDF page image, table with highlighted cells); clicking a citation marker on the canvas or a badge opens it.
**Accept:** e2e: click a red number on the mock slide, see the passage, see the badge explanation.

### D8. Checks, notes, history, exports

**Do:** Checks panel grouped by slide and rule with jump; notes editor; history panel with snapshot thumbnails, restore, revert turn; Exports panel with presets, run, open in PowerPoint, reveal.
**Accept:** e2e for each; real-backend integration test exports pptx and pdf from the test project.

### D9. Skills panel and settings polish

**Do:** skills list with source and enable toggles; skill script approvals routed through cards; audit export.
**Accept:** e2e in mock; integration test with a fixture skill.

### D10. Installers and bundling

**Do:** Windows MSI (WiX via Tauri) and macOS PKG and DMG (signed when certificates are configured in CI secrets; unsigned builds otherwise), embedding the bundled runtime from W3, the embedding model, and the kernel and SDK wheels; LibreOffice as a separate optional download artifact per platform (installed into `<home>/libreoffice/` by a small installer step, never fetched by the app at run time); first-run check that reports engine, sandbox, and embedding status.
**Accept:** CI produces installers; a CI job installs each on a clean runner, launches the app headless (Windows: `tauri-driver` with WebView2; macOS: launch and query via the smoke harness) with network disabled for the app process, opens a fixture project, renders, and exports a PDF with LibreOffice installed from the artifact.

### D11. Smoke tests

**Do:** port Analyst's `apps/desktop/smoke` approach for Windows and macOS: create a project from a fixture, add references, run a scripted chat against the fake model server, accept a proposal, export pptx, screenshot.
**Accept:** smoke job green on both OSes.

### 6.12 Phase 1 exit

Phase 1 packages accepted, and the owner builds and exports a real deck for work in the app, finishing only in PowerPoint (VISION roadmap), recorded in `docs/notes/phase1-exit.md`.

## 7. CI summary

| job | OS | runs |
|---|---|---|
| rust | windows, macos, ubuntu | fmt, clippy, test |
| python | windows, macos, ubuntu | ruff, pytest (kernel, sdk) |
| licenses | ubuntu | cargo-deny, pip-licenses, license-checker |
| schemas | ubuntu | regenerate schemas and TS types, fail on diff |
| sandbox | windows, macos | S1 tests |
| render | windows, macos, ubuntu | renderer, diagrams, checker, LibreOffice fidelity tests; artifacts: rendered PNGs |
| loop | windows, macos, ubuntu | CLI walk-through and eval tasks against `fake_model_server.py` |
| powerpoint | self-hosted `powerpoint` label | PowerPoint engine tests and repair-prompt check (optional) |
| desktop-frontend | ubuntu | typecheck, lint, vitest, build, Playwright (mock) |
| desktop | windows, macos | cargo build and test of the Tauri crate |
| installers | windows, macos | D10 build and clean-install test (on tags and nightly) |
| smoke | windows, macos | D11 |

## 8. Risks and how the plan handles them

| Risk | Handling |
|---|---|
| AppContainer complexity on Windows | Spike S1 first; resource-limits fallback is badged and refusable, so the product still works while it matures |
| Overflow measurement disagrees with PowerPoint | W7 accuracy target and notes; `fit.overflow` can be lowered to `warn` per project; fidelity render is the final check |
| PowerPoint automation unavailable in CI | LibreOffice covers CI; PowerPoint paths mocked plus optional self-hosted runner; manual exit checks |
| Eject not byte-equivalent | Verification compares normalized slide XML, not bytes; switch refused when unverified |
| Bundled embedding model too slow or weak | W9 evaluation note; endpoint option already in config; FTS keeps working alone |
| Large installers | LibreOffice separate; runtime and model sizes recorded per build in CI output |
| Gateway quirks | Unknown-field downgrade and capability records; owner-run evals against the real gateway |

## 9. What the coding agent needs from the owner

- Two or three real corporate templates for W4 evidence (kept out of the repository).
- An endpoint and key for a Claude model through the API or the internal gateway, and one open-weight endpoint meeting the baseline, for owner-run evaluations (never committed; provided through environment variables in a local run).
- Optionally, a Windows machine with PowerPoint registered as a self-hosted runner with the `powerpoint` label.
- Code-signing certificates for Windows and macOS when installers are to be distributed.
- Approval before the shared-repository extraction (section 4) and before any change to the Analyst repository.
