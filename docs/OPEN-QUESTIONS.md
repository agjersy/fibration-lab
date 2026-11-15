# Decisions and Open Questions

*Status: workshop draft (2026-11-15). Everything below blocks the build spec unless marked otherwise. Each question has options, a recommendation, and what is needed to decide. Numbering uses a `P` prefix so it does not collide with Personal Analyst's OQ numbers. Companion documents: [VISION.md](VISION.md), [ARCHITECTURE.md](ARCHITECTURE.md).*

## 1. Inherited from Personal Analyst (proposed as settled)

| Item | Proposed resolution | Why it carries over |
|---|---|---|
| Desktop shell | Tauri 2, Rust core, React renderer, typed IPC with `ts-rs`, mock backend for headless UI development | Same family, same feel, proven in Analyst phase 1 |
| Model agnosticism | No model-specific code paths; capability probing; evaluation suite as the compatibility report | Enterprise users get whichever models their organization approves |
| Edit gating | Review by default, scope declaration, conflict detection by hash, checkpoints per turn | The same "you own it" control model |
| Persistence | SQLite, JSONL, TOML | Same tooling and patterns |
| Licensing | Permissive dependencies only; private repository for now | Keeps both licensing options open |
| UI conventions | Analyst theme tokens, density, dockview layout, Zed-style palette and keymap | Seamless switching between the two apps |
| MCP | Client for stdio and streamable HTTP via `rmcp`, built-in tools on the same schema | Connectors to enterprise systems later |

## 2. Decided in the workshop

| Item | Decision | Where it landed |
|---|---|---|
| OQ-P1 Platforms | Windows and macOS are first-class from phase 1 (MSI and PKG/DMG installers, Windows and macOS CI runners). Linux builds from the same code and is supported in phase 4. | ARCHITECTURE.md P2, sections 3, 12 |
| OQ-P2 Endpoints and first user | First-version endpoints: the Anthropic API, internal API gateways (Anthropic Messages or OpenAI-compatible), and open-weight models on internal GPU servers; Bedrock, Vertex, and Foundry are later auth back ends on the same adapter. The app is always a desktop app calling a server. First user is you, but enterprise-ready from phase 0 (offline bundling, proxy and CA, audit log); managed configuration and organization libraries in phase 3. | ARCHITECTURE.md P6, P12, section 11 |
| OQ-P3 Deck source of truth | Two project modes. Structured (default): typed deck document on template layouts, deterministic renderer, native `process`, `timeline`, `matrix`, and `hierarchy` primitives, plus per-slide script-generated slides as an escape hatch. Code generation (switchable per project): the deck is a sandboxed `python-pptx` build script using the shared deck SDK, checked on its output. Structured → code is a lossless eject; code → structured is a revert to the pre-eject snapshot or a best-effort import. Code execution is therefore required (sandbox details in OQ-P7). | ARCHITECTURE.md P3, P11, sections 5, 5.1, 7.3 |
| OQ-P4 In-app editing | Content-level editing in structured mode: text in place, layout change, reorder/duplicate/delete, locks on slides and elements, chart and table data, speaker notes, diagram primitive content. No free move/resize or styling; final pixel polish happens in PowerPoint after export. In code mode, editing is editing the build script. | ARCHITECTURE.md sections 5.1, 14 |
| OQ-P5 Round-trip and import | Phase 1 is one-way: export is final, but every export carries stable element ids. Structured-mode re-import of PowerPoint edits (text, order, layout, chart data on known elements; unknown changes kept as locked foreign elements) in phase 2. Code-mode re-import later, best-effort, as a reviewable script patch. Existing decks not made by the app are references only; a best-effort 'convert to project' importer comes later (it also serves code → structured switching). | ARCHITECTURE.md sections 5, 5.1, 10 |
| OQ-P6 Rendering engine | Fidelity render chain: PowerPoint automation when installed (COM on Windows, AppleScript on macOS), else a bundled LibreOffice shipped as a separately installable component, else a system LibreOffice. Structured mode keeps the live HTML preview for editing. Without any engine: no PDF/PNG export and no code-mode previews, stated in the UI. CI uses LibreOffice; a Windows runner with PowerPoint is used where available. | ARCHITECTURE.md P5, sections 3, 10 |
| OQ-P7 Skills and code sandbox | Full Agent Skills including scripts, run by the app (ask by default, deniable by config). One OS-native sandbox for all code (builds, generated slides, skill scripts): macOS `sandbox-exec`, Windows AppContainer + job object, Linux `bubblewrap` in phase 4; no network, writes only to scratch and output; badged fallback to resource limits. Code uses a fixed, pinned package set bundled offline in the installer. | ARCHITECTURE.md P7, P11, sections 12, 12.1 |
| OQ-P8 References | Always copied into the project (content-addressed, versioned). v1 formats: PDF, DOCX, PPTX, XLSX/CSV, images, Markdown/text, email (`.eml`, `.msg`), HTML and saved pages, Personal Analyst figures and tables (only imported items are copied). No OCR in v1. No web access in v1; later through MCP. Reference sets are large, so hybrid search (FTS5 + bundled offline embedding model, endpoint optional) is in v1. | ARCHITECTURE.md P8, section 8 |
| OQ-P9 Chart data | Native charts take their data from reference ranges or extracted tables through a declarative, closed set of transforms (filter, sort, top-N, group aggregates, pivot, percent change, share, cumulative, scaling, rounding) run deterministically by the kernel and stored with the chart as provenance; a checker rule recomputes them. Real analysis happens in Personal Analyst; code mode and generated slides remain available for anything else. | ARCHITECTURE.md sections 5, 9 |
| OQ-P10 Checkpoints | App-owned, append-only, content-addressed snapshot store in `.present/history/` (per turn, per mode switch, per export); no git dependency or integration. | ARCHITECTURE.md P10, section 13 |
| OQ-P11 Code sharing and skill format | A third repository of shared Rust crates (`wb-providers`, `wb-policy`, `wb-transcript`, `wb-skills`, `wb-mcp`, `wb-sandbox`) and a UI package (`@wb/ui`), consumed by both apps as tag-pinned git dependencies and extracted gradually, provider layer and policy engine first. Both apps use the Agent Skills `SKILL.md` format; Analyst keeps reading its legacy front matter during a transition. | ARCHITECTURE.md P14, section 18 |

## 3. Open

### OQ-P12. Name

**Options.** "Personal Present" (binary `present`, config `~/.present`), following "Personal Analyst". Alternatives if the product is aimed at organizations: a name without "Personal".

**Decide.** Keep the parallel name?

### OQ-P13. Model baseline — Does not block phase 0

**Recommendation.** Native tool calling, streaming, at least 128k context, vision recommended. Evaluation suite run against at least one Claude deployment on a cloud platform and one open-weight model.

**Decide.** Which concrete endpoints should the evaluation suite target first?

## 4. Evidence to gather in phase 0

- Template coverage: run the template inspector over several real corporate templates you use; count layouts whose placeholders map cleanly.
- Overflow measurement accuracy: compare the checker's verdicts with PowerPoint and LibreOffice renders on a corpus of slides.
- LibreOffice versus PowerPoint render differences on the same templates.
- How often the agent needs generated slides on real briefs, and how often code mode output stays on-template when the build script uses the SDK.
- Retrieval quality of the bundled embedding model on your real reference sets, versus FTS alone and versus an endpoint model if one becomes available.
- Gateway and open-weight server behavior on the same tasks (tool calling, document citations, caching, unknown-field handling) through the shared provider layer.
