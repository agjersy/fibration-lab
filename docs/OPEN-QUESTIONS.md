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
| OQ-P1 Platforms | Windows and macOS are first-class from phase 1 (MSI and PKG/DMG installers, Windows and macOS CI runners). Linux builds from the same code and is supported in phase 4. Skill-script isolation on Windows is weaker and badged. | ARCHITECTURE.md P2, sections 3, 12 |
| OQ-P2 Endpoints and first user | First-version endpoints: the Anthropic API, internal API gateways (Anthropic Messages or OpenAI-compatible), and open-weight models on internal GPU servers; Bedrock, Vertex, and Foundry are later auth back ends on the same adapter. The app is always a desktop app calling a server. First user is you, but enterprise-ready from phase 0 (offline bundling, proxy and CA, audit log); managed configuration and organization libraries in phase 3. | ARCHITECTURE.md P6, P12, section 11 |

## 3. Open

### OQ-P3. Source of truth for a deck

**Options.**
- (a) **Code generation.** The agent writes Python (`python-pptx`) or JavaScript (PptxGenJS) that builds the `.pptx`, run in a sandbox. This is how Anthropic's own pptx skill and most AI deck tools work. Maximal expressiveness; weak template compliance; diffs are code diffs; every change re-runs code; requires arbitrary code execution.
- (b) **Typed deck document.** The agent edits a JSON deck document through tools; a deterministic renderer produces the `.pptx` on the template. Template compliance and checks by construction; slide-level diffs; editing in the app is straightforward; expressiveness limited to the schema plus images and free elements.
- (c) **Direct OOXML editing.** The agent edits the XML of a `.pptx` through tools. Round-trip with PowerPoint is natural; the model must handle OOXML, which is verbose and error-prone; checking is hard.

**Recommendation.** (b), with a planned escape hatch: native diagram primitives in the schema, and images produced by skill scripts. It is the analogue of Analyst's choice to make every output checkable, and it is what makes "the template is the contract" enforceable.

**Decide.** Is the expressiveness limit acceptable for your decks? Which slide types do you build most often that a placeholder-plus-chart-plus-table schema would not cover (process diagrams, timelines, 2×2 matrices, maps, org charts)?

### OQ-P4. How much editing in the app

**Options.** (a) Text, layout, order, locks, chart data, notes in the app; everything else in PowerPoint after export. (b) A fuller canvas editor (move and resize, shapes, styling). (c) Chat-only changes; no direct editing.

**Recommendation.** (a). PowerPoint is the best slide editor and the user already owns it. The app's job is to reach a good deck quickly and verifiably; the last few percent of polish happens in PowerPoint.

**Decide.** Is "finish in PowerPoint" the expected workflow, or should the app be where the deck lives until it is presented?

### OQ-P5. Round-trip: bringing PowerPoint edits back

**Options.** (a) One-way: export is final; further agent work starts from the deck document and loses PowerPoint edits. (b) Re-import: stable ids embedded in the export let the app read a `.pptx` edited in PowerPoint back into the deck document, mapping text, order, layout, and chart data changes on known elements, and keeping unknown changes as locked "foreign" elements. (c) Import any `.pptx` (not only ones the app exported) as a starting deck.

**Recommendation.** (a) in phase 1 with stable ids written from day one, (b) in phase 2. (c) as a best-effort "convert to project" later; arbitrary decks rarely map cleanly onto a template's layouts.

**Decide.** How often would you go back to the agent after editing in PowerPoint? Do you want to bring existing decks in as a starting point, or only as references?

### OQ-P6. Rendering engine for previews, PDF, and images

**Options.** (a) Bundle LibreOffice headless as an optional component (~300 MB, MPL-2.0 as an unmodified separate program; good but imperfect fidelity). (b) Use PowerPoint automation when installed (perfect fidelity; Windows COM is robust, macOS AppleScript is slower and needs automation permission; never in CI without a licensed runner). (c) Live HTML preview only, no fidelity render; PDF only through PowerPoint. (d) Both (a) and (b), PowerPoint first.

**Recommendation.** (d), with the live HTML preview for editing and the fidelity render for thumbnails and exports. Keep the LibreOffice component separately installable so locked-down environments can omit it.

**Decide.** Is a ~300 MB optional component acceptable? Will PowerPoint be installed on every machine that runs the app (then PowerPoint-first is nearly free)?

### OQ-P7. Skills and code execution

**Options.** (a) Instruction-only skills (`SKILL.md` and resources, no scripts). (b) Full Agent Skills including scripts, run locally in a sandbox, approved per policy. (c) Full skills plus a general sandboxed Python tool, as in Analyst.

**Recommendation.** (b). Provider-hosted skills do not exist on gateways or open-weight servers (and not on Bedrock or Vertex later), so the app has to run them; scripts are what make skills useful for charts and diagrams. A general code tool (c) is not needed for decks and is the hardest thing to justify to an enterprise security review. On Windows, scripts run with a restricted token and a job object and are badged as weaker isolation; a managed config can disable scripts entirely.

**Decide.** Do you already have skills (yours or your organization's) you want to run? Do they include scripts?

### OQ-P8. References: scope and handling

**Options and recommendations.**
- Linked versus copied: link with hash by default, copy on request (self-contained project). *Decide:* do projects need to be portable folders you can hand to a colleague?
- Formats in v1: PDF, DOCX, PPTX, XLSX/CSV, images, Markdown/text. *Decide:* anything else essential (email `.msg`/`.eml`, HTML, scanned PDFs needing OCR)?
- Web URLs: off by default; possible later through an MCP connector so enterprise policy controls it. *Decide:* do you need the agent to read web pages?
- Semantic search: only with an approved embedding endpoint; FTS otherwise. *Decide:* is there an embedding model available in your environment?
- Size: what is a typical reference set (a few documents, or hundreds of pages)?

### OQ-P9. Charts and data

**Options.** (a) Native PowerPoint charts from tables in references (spreadsheets, extracted tables) only. (b) Plus images of figures from Personal Analyst or skill scripts. (c) Plus in-app computation (the agent aggregates data with code before charting).

**Recommendation.** (a) + (b). Computation belongs in Personal Analyst, which is built for verified analysis; Present consumes its outputs with provenance. Simple transforms (filter, sort, top-N, sum by group) can be declarative operations in the chart spec so no code is needed.

**Decide.** Is that division of labor right for you, or do you expect to build decks from raw data without going through Analyst?

### OQ-P10. Checkpoints without git

**Options.** (a) System `git` like Analyst. (b) Embedded `gitoxide`. (c) App-owned content-addressed snapshot store.

**Recommendation.** (c). Enterprise Windows machines often lack `git`, users are not assumed to know it, and the deck document is small. Users can still put a project under git themselves.

**Decide.** Any reason to require git (for example, you want decks in your analysis repos)?

### OQ-P11. Code sharing with Personal Analyst

**Options.** (a) Copy what is needed and let the two diverge. (b) Extract shared crates (`protocol` patterns, providers, policy, transcript, skills, MCP, UI theme and chat components) into a shared workspace or repository that both apps depend on. (c) One monorepo with two apps.

**Recommendation.** (b), done gradually: Present's phase 0 starts by extracting the provider layer (the two adapters are identical in both apps; proxy and private-CA handling added for Present benefits Analyst too) and the policy engine. Adopting the Agent Skills `SKILL.md` format in Analyst as well keeps one skill format across both apps.

**Decide.** Are you willing to change Analyst's skill format and move shared code out of its repository? Monorepo or separate repositories?

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
- How often the agent needs something outside the deck schema on real briefs (sizes OQ-P3's escape hatch).
- Gateway and open-weight server behavior on the same tasks (tool calling, document citations, caching, unknown-field handling) through the shared provider layer.
