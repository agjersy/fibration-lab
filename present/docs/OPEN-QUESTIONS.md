# Decisions and Open Questions

> **Historical design note.** This document describes Personal Present before it was folded into Fibration Lab. Where it conflicts with the documents in [`docs/`](../../docs/) (one core, one `fibration` CLI and desktop app, decks under `decks/<slug>/` in the project, git checkpoints instead of snapshots, the artifact index instead of the scan/import bridge, `~/.fibration` instead of `~/.present`), `docs/` wins. `PRESENT_HOME` and the `present` binary were removed.

*Status: all workshop questions decided (2026-11-15). Nothing blocks the build spec. Numbering uses a `P` prefix so it does not collide with Fibration Lab's OQ numbers. Companion documents: [VISION.md](VISION.md), [ARCHITECTURE.md](ARCHITECTURE.md).*

## 1. Inherited from Fibration Lab (settled)

| Item | Proposed resolution | Why it carries over |
|---|---|---|
| Desktop shell | Tauri 2, Rust core, React renderer, typed IPC with `ts-rs`, mock backend for headless UI development | Same family, same feel, proven in Analyst phase 1 |
| Model agnosticism | No model-specific code paths; capability probing; evaluation suite as the compatibility report | Enterprise users get whichever models their organization approves |
| Edit gating | Review by default, scope declaration, conflict detection by hash, checkpoints per turn | The same "you own it" control model |
| Persistence | SQLite, JSONL, TOML | Same tooling and patterns |
| Licensing | Permissive dependencies only; private repository for now | Keeps both licensing options open |
| UI conventions | Analyst theme tokens, density, dockview layout, Zed-style palette and keymap | Seamless switching between the two apps |
| MCP | Not adopted: the app reaches the network only for model endpoints, package installs, and SSH targets | Built-in tools only |

## 2. Decided in the workshop

| Item | Decision | Where it landed |
|---|---|---|
| OQ-P1 Platforms | Windows and macOS are first-class from phase 1 (MSI and PKG/DMG installers, Windows and macOS CI runners). Linux builds from the same code and is supported in phase 4. | ARCHITECTURE.md P2, sections 3, 12 |
| OQ-P2 Endpoints and first user | First-version endpoints: the Anthropic API, internal API gateways (Anthropic Messages or OpenAI-compatible), and open-weight models on internal GPU servers; Bedrock, Vertex, and Foundry are later auth back ends on the same adapter. The app is always a desktop app calling a server. First user is you, but enterprise-ready from phase 0 (offline bundling, audit log; proxy and custom CA support were removed); managed configuration and organization libraries in phase 3. | ARCHITECTURE.md P6, P12, section 11 |
| OQ-P3 Deck source of truth | Two project modes. Structured (default): typed deck document on template layouts, deterministic renderer, native `process`, `timeline`, `matrix`, and `hierarchy` primitives, plus per-slide script-generated slides as an escape hatch. Code generation (switchable per project): the deck is a sandboxed `python-pptx` build script using the shared deck SDK, checked on its output. Structured → code is a lossless eject; code → structured is a revert to the pre-eject snapshot or a best-effort import. Code execution is therefore required (sandbox details in OQ-P7). | ARCHITECTURE.md P3, P11, sections 5, 5.1, 7.3 |
| OQ-P4 In-app editing | Content-level editing in structured mode: text in place, layout change, reorder/duplicate/delete, locks on slides and elements, chart and table data, speaker notes, diagram primitive content. No free move/resize or styling; final pixel polish happens in PowerPoint after export. In code mode, editing is editing the build script. | ARCHITECTURE.md sections 5.1, 14 |
| OQ-P5 Round-trip and import | Phase 1 is one-way: export is final, but every export carries stable element ids. Structured-mode re-import of PowerPoint edits (text, order, layout, chart data on known elements; unknown changes kept as locked foreign elements) in phase 2. Code-mode re-import later, best-effort, as a reviewable script patch. Existing decks not made by the app are references only; a best-effort 'convert to project' importer comes later (it also serves code → structured switching). | ARCHITECTURE.md sections 5, 5.1, 10 |
| OQ-P6 Rendering engine | Fidelity render chain: PowerPoint automation when installed (COM on Windows, AppleScript on macOS), else a bundled LibreOffice shipped as a separately installable component, else a system LibreOffice. Structured mode keeps the live HTML preview for editing. Without any engine: no PDF/PNG export and no code-mode previews, stated in the UI. CI uses LibreOffice; a Windows runner with PowerPoint is used where available. | ARCHITECTURE.md P5, sections 3, 10 |
| OQ-P7 Skills and code sandbox | Full Agent Skills including scripts, run by the app (ask by default, deniable by config). One OS-native sandbox for all code (builds, generated slides, skill scripts): macOS `sandbox-exec`, Windows AppContainer + job object, Linux `bubblewrap` in phase 4; no network, writes only to scratch and output; badged fallback to resource limits. Code uses a fixed, pinned package set bundled offline in the installer. | ARCHITECTURE.md P7, P11, sections 12, 12.1 |
| OQ-P8 References | Always copied into the project (content-addressed, versioned). v1 formats: PDF, DOCX, PPTX, XLSX/CSV, images, Markdown/text, email (`.eml`, `.msg`), HTML and saved pages, Fibration Lab figures and tables (only imported items are copied). No OCR in v1. No web access: references come from the file system. Reference sets are large, so hybrid search (FTS5 + bundled offline embedding model, endpoint optional) is in v1. | ARCHITECTURE.md P8, section 8 |
| OQ-P9 Chart data | Native charts take their data from reference ranges or extracted tables through a declarative, closed set of transforms (filter, sort, top-N, group aggregates, pivot, percent change, share, cumulative, scaling, rounding) run deterministically by the kernel and stored with the chart as provenance; a checker rule recomputes them. Real analysis happens in Fibration Lab; code mode and generated slides remain available for anything else. | ARCHITECTURE.md sections 5, 9 |
| OQ-P10 Checkpoints | App-owned, append-only, content-addressed snapshot store in `.present/history/` (per turn, per mode switch, per export); no git dependency or integration. | ARCHITECTURE.md P10, section 13 |
| OQ-P11 Code sharing and skill format | A third repository of shared Rust crates (`wb-providers`, `wb-policy`, `wb-transcript`, `wb-skills`, `wb-sandbox`) and a UI package (`@wb/ui`), consumed by both apps as tag-pinned git dependencies and extracted gradually, provider layer and policy engine first. Both apps use the Agent Skills `SKILL.md` format; Analyst keeps reading its legacy front matter during a transition. | ARCHITECTURE.md P14, section 18 |
| OQ-P12 Name | "Personal Present": binary `present`, config `~/.present` (`%APPDATA%\Present` on Windows), bundle id `dev.personal-present.app`. | ARCHITECTURE.md P15 |
| OQ-P13 Model baseline | Native tool calling, streaming, at least 128k context, and vision required. A model without native tool calling is rejected; models below the context or vision baseline run as a flagged degraded tier the evaluation suite does not certify. Certification targets: Claude through the Anthropic API or the internal gateway, and at least one baseline-meeting open-weight model on internal servers. | ARCHITECTURE.md P16, sections 7.4, 11 |

## 3. Evidence to gather in phase 0

- Template coverage: run the template inspector over several real corporate templates you use; count layouts whose placeholders map cleanly.
- Overflow measurement accuracy: compare the checker's verdicts with PowerPoint and LibreOffice renders on a corpus of slides.
- LibreOffice versus PowerPoint render differences on the same templates.
- How often the agent needs generated slides on real briefs, and how often code mode output stays on-template when the build script uses the SDK.
- Retrieval quality of the bundled embedding model on your real reference sets, versus FTS alone and versus an endpoint model if one becomes available.
- Which open-weight models available on your internal servers meet the 128k + vision baseline, and how they score on the suite.
- Gateway and open-weight server behavior on the same tasks (tool calling, document citations, caching, unknown-field handling) through the shared provider layer.

## 4. Questions from the build

Recorded while implementing phase 0 (BUILD-SPEC rule 10). Each lists what was built and what is left to confirm.

### Deviations from the contracts (PROTOCOL.md was updated where noted)

| Topic | What was built | Why |
|---|---|---|
| Vector search | Embeddings are stored as blobs in SQLite and searched by exact cosine similarity in Rust, not with the `sqlite-vec` extension | No native extension to ship on three platforms; exact search over a project's chunk counts takes milliseconds and is deterministic |
| Test fonts | Liberation Sans, Carlito, and DejaVu Serif are vendored instead of Noto | Available offline under open licenses; metric-compatible with Arial and Calibri |
| Sandbox placement | The macOS `sandbox-exec` profile and Linux `bwrap` wrappers live in the Python kernel (ported from Analyst's kernel); only the Windows AppContainer launcher is in the `wb-sandbox` crate | The kernel launches the sandboxed process, and Analyst's proven profile generator is Python |
| Ids | Slide, element, reference, and snapshot id patterns are looser than first specified | The SDK emits ids such as `e-raw-1` and `e-ftr` |
| Template description | Gains `default_table_style`, placeholder `anchor`, and per-level `mar_l` | Needed for native tables and text measurement |
| Autofit | The SDK writes PowerPoint's shrink-to-fit scale into the file at save time | PowerPoint only recomputes it when text is edited, so generated files would otherwise overflow |
| Hex colors | The slide schema accepts theme color names only; a literal color cannot be stored in a structured deck | Stronger than the planned `tpl.theme-colors` warning, which still applies to code-mode output |
| Outline storage | An accepted outline is stored at `.present/outline.json` and shown to the agent in the project map | The contract did not say where it lives |
| Transcript | Events gain `compaction` and `decision.modified`; `proposal` writes `targets` (reads `slides`) | Needed to replay a compacted conversation and to record accept-with-edits |
| Evaluation fixtures | A fixture is `TEMPLATE` + `sources/` + optional `deck/`, `assets/`, `project.toml`, built into a fresh project by the harness | Avoids committing copied templates and pre-ingested references |
| Proxies and trust | No proxy is used (proxy environment variables and system settings are ignored) and no custom CA bundle can be added; endpoints are reached directly and verified against the bundled web trust roots | The network rule: only model endpoints, package installs, and SSH targets, through the network gate |
| Project events | `project_subscribe` has no matching unsubscribe; the channel ends when the project or the window closes, and subscribing again from the same window replaces the earlier channel | One channel per project per window is all the renderer needs |
| Fidelity thumbnails | `render_fidelity` renders the whole deck in one pass whatever `slides` says | The engines convert a file at a time; per-slide renders would cost the same |
| Persisted policy | `project_set_policy` with `persist: true` rewrites `.present/project.toml` from its parsed form, which drops comments | No comment-preserving TOML writer in use |
| Export presets | `export_run` accepts `preset` and `options` and ignores them | They arrive with the export dialog (BUILD-SPEC D8) |
| Scratch render | Checks, slide renders, and builds take a per-project lock around the shared scratch `.pptx` | Two checks running at once read each other's half-written file |
| Sandbox environment (Windows) | Sandboxed code keeps the standard Windows folder variables (`LOCALAPPDATA`, `APPDATA`, `PROGRAMDATA`, and similar) | Windows refuses to start an AppContainer process without them; they name folders, not secrets |
| Combination charts | Bar-plus-line charts share one value axis; `axis: "secondary"` is accepted but not yet rendered | Not implemented yet |

### Not verified in this environment

- **Windows AppContainer sandbox.** The launcher now runs on the Windows CI job. Two faults found there were fixed (a system fonts folder whose permissions cannot be changed; a scrubbed environment Windows would not start a process with). Whether the isolation tests (BUILD-SPEC S1 a to e) pass there is recorded in the CI run for the latest commit, not here. Launch latency with a bundled runtime is unmeasured; `docs/notes/sandbox-spike.md` explains the likely mitigation (a permanent read entry on the runtime at install time).
- **macOS `sandbox-exec`.** The escape tests pass on the macOS CI job.
- **Linux `bwrap`.** The escape tests pass locally and on the Linux CI job. Inside the sandbox the home directory is a private, discarded tmpfs, so a write there succeeds in the sandbox and never reaches the host.
- **PowerPoint.** Both automation paths are tested with a mocked runner only. No exported file has been opened in PowerPoint; the phase 0 exit check needs a person with PowerPoint on Windows and macOS.
- **ECMA-376 schema validation.** The checker validates when the XSDs are present under `vendor/ooxml-schemas`; they have not been fetched, pending confirmation that they may be redistributed.
- **Embedding model.** No model could be downloaded here. The runner is tested against a tiny hand-built ONNX model; the candidate (`multilingual-e5-small`, int8 build) and its license, size, and quality are unconfirmed. Search works without it (full-text only).
- **Real models.** Every agent test uses a scripted model. No run against Claude or an open-weight model has been made; six of the eight evaluation tasks need one.
- **Runtime bundling.** `tools/bundle_runtime.py` builds a self-contained runtime and the CLI walk-through passes on it, on Linux only (about 555 MB before compression, of which pandas, onnxruntime, numpy, and matplotlib are about 190 MB). It has not been run on Windows or macOS, and it is not yet part of CI.

### Decisions to confirm

1. Diagram label size is capped at 24 pt even when the template's body text is larger; at the body size small diagrams looked oversized.
2. In review mode the user is asked about every slide change; in auto mode a new slide is applied only when `scope_declare` announced new slides, otherwise the user is asked.
3. Self-review (`slide_render` on every changed slide) is enforced with one reminder per turn, and only when the model has vision and a rendering engine exists.
4. Evaluations run with edits, code, skill scripts, and exports all automatic, so `edits_within_scope` is the only signal for unwanted edits.
