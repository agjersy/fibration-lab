# Personal Present — Application Design and Architecture

> **Historical design note.** This document describes Personal Present before it was folded into Fibration Lab. Where it conflicts with the documents in [`docs/`](../../docs/) (one core, one `fibration` CLI and desktop app, decks under `decks/<slug>/` in the project, git checkpoints instead of snapshots, the artifact index instead of the scan/import bridge, `~/.fibration` instead of `~/.present`), `docs/` wins. `PRESENT_HOME` and the `present` binary were removed.

*Status: workshop decisions closed (2026-11-15); every item in the decision summary is decided. The build is specified in [BUILD-SPEC.md](BUILD-SPEC.md) with contracts in [PROTOCOL.md](PROTOCOL.md) and [IPC.md](IPC.md). Companion documents: [VISION.md](VISION.md), [OPEN-QUESTIONS.md](OPEN-QUESTIONS.md). Reference architecture: Fibration Lab `docs/ARCHITECTURE.md`; sections marked **(shared)** reuse its design and are only summarized here.*

## 1. Decision summary

| # | Decision | Resolution | Status |
|---|---|---|---|
| P1 | Desktop shell | Tauri 2, Rust core, React renderer in the OS webview, as in Analyst (D1) | Decided (shared) |
| P2 | Platforms | Windows and macOS first-class from phase 1 (that is where PowerPoint users are); Linux built from the same code and supported in phase 4 | Decided (OQ-P1) |
| P3 | Source of truth for a deck | Two project modes. Structured (default): a typed deck document (JSON, one file per slide) bound to the template's layouts, with native process, timeline, matrix, and hierarchy primitives and optional script-generated slides, rendered deterministically to `.pptx`. Code generation (switchable): the deck is a sandboxed `python-pptx` build script using a shared deck SDK. Structured → code is a lossless eject; code → structured is a revert or a best-effort import | Decided (OQ-P3) |
| P4 | `.pptx` writer | `python-pptx` in an app-owned, bundled Python runtime (the "deck kernel"), driven over JSON-RPC like Analyst's kernel | Decided |
| P5 | Preview and PDF/PNG export | Live HTML/SVG preview from the deck document (structured mode), plus a fidelity render of the actual `.pptx` by PowerPoint automation when installed, else a separately installable bundled LibreOffice, else a system LibreOffice | Decided (OQ-P6) |
| P6 | Model access | Analyst's `ModelClient` with its two wire adapters (Anthropic Messages, OpenAI-compatible). First version targets the Anthropic API, internal gateways speaking either format, and open-weight models on internal servers. Cloud-platform auth back ends (Bedrock, Vertex, Foundry) are a later addition behind the same interface | Decided (OQ-P2) |
| P7 | Skills | Full Anthropic Agent Skills (`SKILL.md` + resources + scripts), discovered, loaded, and executed by the app, never by the provider, so skills work identically on every endpoint; scripts in the code sandbox, ask by default | Decided (OQ-P7) |
| P8 | References | Always copied into the project. PDF, DOCX, PPTX, XLSX/CSV, images, Markdown/text, email (`.eml`, `.msg`), HTML, and Fibration Lab outputs. Hybrid search: SQLite FTS5 plus a bundled offline embedding model (endpoint optional). No OCR and no web access in v1. Per-element citations | Decided (OQ-P8) |
| P9 | Edit gating | The shared policy engine (`wb-policy`) applied per slide: review (default), auto-new-slides, auto, deny; the three presets `review`, `auto`, `read-only`; locked slides and elements; scope declaration | Decided (shared) |
| P10 | Checkpoints | App-owned content-addressed snapshot store in `.present/history/`; no git dependency or integration (a user may still version the folder with git) | Decided (OQ-P10) |
| P11 | Code execution | Required: code generation mode, generated slides, and skill scripts all run the bundled Python with a fixed package set in an OS-native sandbox (macOS `sandbox-exec`, Windows AppContainer + job object) with no network. No general shell or REPL tool | Decided (OQ-P3, OQ-P7) |
| P12 | Enterprise deployment | Built for you first but enterprise-ready from phase 0: offline bundling with no runtime downloads, a network gate that allows only configured model endpoints (no proxy or custom CA support), local audit log, no telemetry. Managed configuration and organization libraries ship in phase 3 | Decided (OQ-P2) |
| P13 | Persistence | SQLite for indexes, JSONL for transcripts, TOML for config, as in Analyst (D13) | Decided (shared) |
| P14 | Code sharing with Analyst | A third repository of shared Rust crates and a UI package, consumed by both apps as pinned git dependencies, extracted gradually starting with the provider layer and policy engine. One skill format (Agent Skills `SKILL.md`) across both apps | Decided (OQ-P11) |
| P15 | Name | "Personal Present": binary `present`, config `~/.present` (`%APPDATA%\Present` on Windows), bundle id `dev.personal-present.app` | Decided (OQ-P12) |
| P16 | Model baseline | Tool calling (required: a model without it is rejected), streaming, >=128k context, vision; models below the context or vision baseline run as a flagged degraded tier | Decided (OQ-P13) |

## 2. System overview

```mermaid
flowchart LR
  subgraph UI["Renderer (React, system webview)"]
    Grid[Slide grid + outline]
    Canvas[Slide canvas + live preview]
    Chat[Agent chat]
    Refs[References + source lens]
    Checks[Checks]
    Exp[Exports]
  end

  subgraph Core["Core (Tauri backend, Rust)"]
    PM[Project manager]
    AL[Agent loop]
    PE[Policy engine + slide gating]
    MP[Model providers\nAnthropic Messages | OpenAI-compat]
    TR[Tool registry]
    SK[Skill loader + runner]
    DS[Deck store + snapshots]
    RI[Reference index\nSQLite FTS5]
    AU[Audit log]
    KC[Deck kernel client]
  end

  subgraph Kernel["Deck kernel (bundled Python, local)"]
    TI[Template inspector]
    RD[pptx renderer\npython-pptx]
    EX[Reference extractors]
    CC[Deck checker\nfont metrics]
    FR[Fidelity renderer\nLibreOffice | PowerPoint]
  end

  LLM[(Model endpoints)]
  Files[(Project folder\nreferences on disk)]
  AN[(Fibration Lab repo)]

  UI <-- typed IPC --> Core
  AL --> MP --> LLM
  AL --> TR
  AL --> PE
  AL --> SK
  AL --> DS
  AL --> RI
  MP --> AU
  KC -- stdio JSON-RPC --> Kernel
  DS --> Files
  EX --> Files
  RI --> AN
```

Three tiers, as in Analyst:

- **Renderer.** All visible surfaces. Holds no authority; everything goes through typed IPC. Draws the live slide preview from the deck document and the template description, so editing feels immediate.
- **Core.** Owns projects, the agent loop, policy, model access, skills, references, the deck store, snapshots, and the audit log.
- **Deck kernel.** A long-lived local Python process from a bundled runtime. Everything that needs the OOXML ecosystem lives here: reading templates, writing `.pptx`, extracting text and tables from reference files, measuring text against real fonts, and driving the fidelity renderer. Unlike Analyst there is no remote target: everything is local. (A remote kernel is possible later behind the same transport trait.)

## 3. Desktop stack

Same as Analyst (section 3 there) unless listed:

| Layer | Choice | Difference from Analyst |
|---|---|---|
| Shell | Tauri 2 | Windows uses WebView2 (preinstalled on Windows 10/11, enterprise-deployable). Installers: MSI (WiX, deployable with Intune/SCCM), signed PKG and DMG (Jamf), DEB/RPM later. |
| Slide rendering in the UI | React components producing absolutely positioned HTML (text) and SVG (shapes, charts) in EMU-scaled coordinates, with template fonts loaded from the system | New. Replaces the editor/figure panels' role. |
| Charts in the UI preview | A small chart renderer over the deck's chart data (Vega-Lite or hand-written SVG) styled from the template theme colors | New. The exported chart is a native PowerPoint chart regardless. |
| Python runtime | `python-build-standalone` + pinned wheels, bundled in the installer; no download at first run | Analyst fetches with `uv` at runtime; enterprise offline installs cannot. |
| Fidelity renderer | LibreOffice headless (`soffice --convert-to pdf`) bundled as an optional component or found on the system; PowerPoint automation (COM on Windows, AppleScript on macOS) when PowerPoint is installed | New. See OQ-P6. |
| Network | The network gate (`wb-net`) builds every HTTP request and refuses any host that is not a configured model endpoint. `rustls` with the bundled web trust roots, direct connections: no proxy, custom CA bundle, or client certificate support | Shared with Analyst. The app reaches the network only for model endpoints, package installs, and SSH targets. |
| CodeMirror editor | Included for code generation mode, generated slides, and skills, with per-hunk review as in Analyst | Hidden in structured mode except as a developer JSON view. |
| Terminal, SSH | Not included | The user is not assumed to be a programmer. |

Licensing rule from Analyst kept: permissive dependencies only (no GPL or AGPL). Consequences here: PyMuPDF (AGPL) is excluded, `pypdfium2` (Apache/BSD) is used for PDF; LibreOffice (MPL-2.0) is acceptable only as an unmodified separate program, and is optional.

## 4. Domain model

| Entity | Definition | Owned by |
|---|---|---|
| Workspace | Providers, models, global skills, template library, list of projects, managed policy. | `~/.present/` + managed config |
| Project | One deck + one template + its references + one or more conversations + layout + export history. One tab per project. | project folder |
| Template | A `.potx`/`.pptx` whose slide masters, layouts, placeholders, and theme define what the deck may use. Inspected once into a template description. | project `template/`, or the library |
| Template description | Machine-readable summary of a template: layouts with placeholder types, positions, sizes, and text styles per level; theme colors and fonts; slide size. Given to the agent as the layout catalog and to the renderer for previews. | computed, cached |
| Deck | Ordered sections and slides plus deck-level metadata (title, author, footer text, classification). | `deck/deck.json` |
| Slide | A layout reference, the content of each placeholder, optional extra elements, speaker notes, lock state, and a stable id. | `deck/slides/<id>.json` |
| Element | Text block, image, chart, table, or (later) diagram, either in a placeholder or positioned freely if the contract allows. Carries citations. | inside the slide |
| Citation | A link from a run of text or a data series to a location in a reference (page, character range, sheet and cell range, slide and shape). | inside the element |
| Reference | A source file (or an Analyst artifact) with extracted text, tables, and images, its hash, and its location. | `references/` (copied, content-addressed); index in SQLite |
| Conversation, Turn, Proposal | As in Analyst. A proposal is a change set over slides. | transcript |
| Snapshot | The deck document before an agent turn, content-addressed. | `.present/history/` |
| Export | A rendered output file with its format, preset, deck snapshot id, and checker result. | `exports/` |
| Skill | Agent Skill folder. | `~/.present/skills/`, project `.present/skills/`, managed library |
| Provider / Model | Endpoint, auth back end, model id, probed capabilities. | config; keys in the environment or `~/.present/.env` |
| Policy | Gating rules, locked keys from managed config. | `.present/project.toml` + managed config |

## 5. The deck document (P3)

This is the central design choice; OQ-P3 sets out the alternatives.

**Decided (structured mode, OQ-P3):** a JSON document, validated against a published JSON Schema, in which a slide is *a layout from the template plus content for its placeholders*. The renderer turns it into `.pptx` deterministically: the same document and template always produce the same file.

```json
{
  "id": "s-7f3a",
  "layout": "Title and Content",
  "placeholders": {
    "title": { "text": [{ "runs": [{ "t": "Margin recovered to 18% on pricing actions" }] }] },
    "body": {
      "text": [
        { "level": 0, "runs": [{ "t": "Gross margin up 3.1 pts quarter on quarter", "cite": ["ref:q3-report#p11:c120-188"] }] },
        { "level": 1, "runs": [{ "t": "Two thirds from list-price changes in EMEA", "cite": ["ref:q3-report#p12"] }] }
      ]
    }
  },
  "elements": [],
  "notes": { "text": [{ "runs": [{ "t": "Pause on the EMEA point; Sarah owns the follow-up." }] }] },
  "locked": false
}
```

Rules:

- **Text** is a list of paragraphs with a level and a list of runs. Run formatting is limited to emphasis (bold, italic, underline), hyperlinks, and theme color references (`accent1`, never a hex value unless the contract allows). Font, size, and spacing come from the layout's placeholder styles; the agent cannot set them unless the contract allows overrides.
- **Charts** are a type (bar, column, line, pie, scatter, area, combo, and their stacked variants), a data source, an optional transform pipeline, and a small set of options (axis titles, labels, legend). Rendered as native PowerPoint charts with an embedded workbook holding the final values, styled from theme colors. Decided (OQ-P9):
  - **Source:** a cell range or named range in a reference spreadsheet, an extracted table from a PDF, DOCX, or PPTX reference, or an imported Fibration Lab table, always with its reference version. Inline literal data is allowed only with a citation per value.
  - **Transforms:** a declarative list of steps from a closed set: `filter`, `select`, `rename`, `sort`, `top_n`, `group_sum` / `group_mean` / `group_count`, `pivot`, `unpivot`, `pct_change`, `share_of_total`, `cumulative`, `scale` (units: thousands, millions, percent), `round`. Executed by the kernel with pandas, deterministic, no user code.
  - **Provenance:** the chart stores source, steps, and the computed values. The source lens on a chart shows the source range, each step, and the resulting table; the checker recomputes the pipeline from the current reference version and flags a chart whose stored values no longer match (`src.chart-stale`).
  - Anything beyond the closed set belongs in Fibration Lab, which returns a table or figure with run provenance, or in a generated slide or code mode, where the computation is reviewed code.
- **Tables** are rows and cells of text, with header row and banding flags; rendered as native tables using the template's table style.
- **Images** reference a file in the project's `assets/` (or an Analyst figure) by hash, with alt text required and a crop and fit mode into the placeholder.
- **Free elements** (outside placeholders) carry an explicit position and size in EMU and are only allowed where the deck contract allows them.
- **Stable ids** are written into every exported `.pptx` from phase 1 (shape names and `p:custDataLst` tags, plus the deck snapshot id in custom document properties) so the phase 2 re-import can map shapes back (decided, OQ-P5). Phase 1 export is one-way.
- **One file per slide** keeps agent proposals, diffs, and snapshots small and readable.

Agent-facing tools operate on this document (section 7.3). The model never sees OOXML.

**Native diagram primitives (decided).** Four families are part of the schema, rendered as grouped, editable PowerPoint shapes and connectors styled from the theme (never SmartArt, whose XML cannot be generated reliably), each with a layout algorithm in the kernel so the agent supplies content, not coordinates:

| Primitive | Variants | Content model |
|---|---|---|
| `process` | chevrons, numbered steps, flowchart with connectors and decisions | ordered nodes, optional edges and branch labels |
| `timeline` | milestones on an axis, roadmap swimlanes, Gantt bars | dated items, optional lanes and spans |
| `matrix` | 2x2 with plotted items, comparison grid with icons and Harvey balls, KPI tiles | axes or rows and columns, cells, scores |
| `hierarchy` | org chart, tree, driver tree | nodes with parent ids, optional values |

**Generated slides (decided).** When neither the schema nor the primitives can express a slide, a single slide can be of kind `generated`: its content is a short Python script, run in the sandbox against the deck SDK (section 5.1), that draws onto that slide's layout. The rest of the deck stays structured. Generated slides are badged in the slide rail, their script is reviewed like code, and the checker runs on their rendered output.

### 5.1 Deck modes: structured and code generation (decided, OQ-P3)

A project is in one of two modes, chosen at creation and switchable later.

| | Structured (default) | Code generation |
|---|---|---|
| Source of truth | Deck document: `deck/deck.json` + `deck/slides/<id>.json` | Build script: `deck/build.py` (plus modules it imports) + `assets/` |
| Who writes the `.pptx` | The deterministic renderer | The script, run in the sandbox by the kernel with the bundled Python, `python-pptx`, and the deck SDK |
| Expressiveness | Schema + primitives + generated slides | Anything `python-pptx` (and raw OOXML through it) can produce |
| Template compliance | By construction, then checked | Checked on the output only |
| Agent tools | `slide.propose_*` on the document | `code.propose_edit` on the script, `deck.build` |
| Review | Slide diff: before/after thumbnails + text diff | Code diff (CodeMirror, per hunk, as in Analyst) + before/after thumbnails of every slide whose output changed |
| Preview | Live HTML preview + fidelity render | Fidelity render only, after each build |
| In-app editing | Content level: text, layout, order, locks, chart, table, and primitive data, notes (OQ-P4) | Edit the script; slide-level locks become "frozen" slides the build must reproduce byte-for-byte in text (checked) |
| Citations | Stored on runs and series in the document | Attached in code through the SDK (`cite(shape_or_run, "ref:...")`), written into the `.pptx` as tags and read back by the checker and source lens |
| Requires code execution | Only for generated slides and skill scripts | Always |

**Deck SDK (`present_sdk`).** A small Python library shipped with the kernel and importable by build scripts and generated slides. It gives code the same footing as the renderer: `deck = open_template()`, `add_slide(layout="Title and Content")`, placeholder access by role, theme colors and fonts by name, the diagram primitives and chart builders the renderer uses, `cite(...)`, and `alt(...)`. Scripts are expected (by the system prompt and a lint rule) to use the SDK rather than hard-coded fonts, colors, and positions, which keeps code-generated decks on-template in practice even though compliance is no longer guaranteed by construction.

**Switching.**

- **Structured → code ("eject").** The renderer has a code back end that emits an equivalent `build.py` written against the SDK: one function per slide, readable, with citations preserved. Running it reproduces the same `.pptx` (verified at switch time by comparing output). Generated slides are inlined. Lossless.
- **Code → structured.** Not generally possible: arbitrary code has no structured equivalent. The app offers (1) reverting to the structured snapshot taken before the eject, discarding code-mode changes, or (2) importing the current output `.pptx` as a new structured deck on a best-effort basis (the later 'convert to project' importer, OQ-P5), with unmapped shapes kept as locked foreign elements. Both are explicit, previewed actions.
- Every switch is a snapshot boundary and is recorded in the transcript; the agent is told the mode in its context.

**Policy.** Code generation mode needs the sandbox (OQ-P7) and can be disabled by configuration (`policy.code_mode = "allow" | "deny"`), as an enterprise managed setting would. Builds have a time limit and no network.


## 6. Templates

**Ingestion.** The kernel opens the template with `python-pptx`, walks masters and layouts, and produces the template description: for each layout its name, the placeholders (type, index, position, size, text style per level, autofit setting), and for the theme its color scheme and major/minor fonts. Layouts whose names are unhelpful ("Layout 7") get a generated description and a thumbnail from the fidelity renderer, so the agent and the user can pick layouts by look and purpose.

**Usage hints.** A template can carry an optional sidecar `template.toml` written by the brand owner: which layouts to use for what, which to avoid, maximum bullets, approved icon and image folders, mandatory slides (title, disclaimer, closing). The sidecar is effectively a skill bound to the template and is shown to the agent with the layout catalog.

**Library.** `~/.present/templates/` plus a managed, read-only organization library path (a file share or a synced folder). Projects copy the template in, so a project folder is self-contained and reproducible.

## 7. Agent loop

**(shared)** Structure, pause-on-ask semantics, compaction, pinned facts, and capability probing follow Analyst section 5.7. Deck-specific parts:

### 7.1 Context assembly

1. System prompt: role, the deck contract (section 9), output conventions (action titles, one message per slide, cite every claim), and the active policy summary.
2. Project map: brief, deck outline (one line per slide: id, layout, title, lock state, check status), template layout catalog, reference list with one-line summaries, user edits since the last turn.
3. Skill descriptions (name + description only); full bodies are loaded on demand through `skill.load` (progressive disclosure).
4. Conversation history, compacted as in Analyst.
5. The user message plus attachments (selected slides, selected reference passages, images).

### 7.2 Modes

| Mode | Tools | Use |
|---|---|---|
| Outline | Read tools, reference tools, `outline.propose` | Storyline: titles, key messages, layout intent, sources. Nothing is rendered. |
| Build | Everything, subject to policy | Create and change slides. |
| Polish | Read tools plus edits restricted to wording, layout swaps, and formatting within the contract; no new slides, no new claims | Final pass without content drift. |

### 7.3 Built-in tools

| Tool | Policy class |
|---|---|
| `deck.outline`, `deck.read_slide`, `deck.search` | read |
| `template.layouts`, `template.layout_detail` | read |
| `refs.list`, `refs.search` (FTS), `refs.read` (bounded passage by location), `refs.table` (sheet range or extracted table), `refs.image` | read |
| `slide.render` (returns a PNG of a slide's fidelity render) | read |
| `check.run` (deck checker, all or some slides) | read |
| `outline.propose` | edit |
| `slide.propose_create`, `slide.propose_update`, `slide.propose_delete`, `slide.propose_move` (structured mode) | edit |
| `code.read`, `code.propose_edit` (code mode, and the script of a generated slide) | edit |
| `deck.build` (run the build script or a generated slide in the sandbox, then render) | run |
| `deck.switch_mode` (always asks) | edit |
| `asset.import` (image from a reference or an Analyst figure into `assets/`) | edit |
| `skill.load`, `skill.read_resource` | read |
| `skill.run_script` | run (OQ-P7) |
| `export.run` | export |

No shell tool, no general code REPL, no file write outside the deck store, no network tool in the first version. Code runs only as a deck build, a generated slide, or a skill script.

### 7.4 Verification built into the loop

- After every accepted slide edit: the checker runs on that slide (overflow measured with the real fonts, contract rules, citation coverage, number check). Errors go back to the model before the user sees "done", at most N automatic fix iterations.
- The agent is required (system prompt, and a turn-summary check that every new or changed slide was rendered and viewed) to call `slide.render` on new slides and review them against a short visual checklist (alignment, density, legibility). Vision is part of the capability baseline, so this step is always on; it is a quality step, not a hard gate, and it needs a fidelity engine (section 10); without one, the live HTML preview is rasterized instead and the card says so.
- Scope declaration: in Build mode the first action on a new task lists the slide ids it will create or change; changes outside it need approval even in auto mode.

## 8. References and source tracing

**Ingestion** (kernel, local): PDF with `pypdfium2` (text with page and character offsets, embedded images; pages without a text layer are flagged and can be sent to a vision-capable model as page images, since OCR is not in v1); DOCX with `python-docx` (paragraphs, headings, tables); PPTX with `python-pptx` (per slide and shape, notes, and charts' data); XLSX with `openpyxl` (sheets, named ranges, cell values; formulas as evaluated values only); CSV; Markdown and plain text; images (with optional captioning by a vision model on request); email: `.eml` with the Python standard library and `.msg` with a small reader over `olefile` (BSD) following the published MS-OXMSG format (subject, sender, recipients, date, body, attachments; GPL-licensed `.msg` libraries are excluded by the licensing rule), with attachments ingested as child references; HTML and saved web pages (`.html`, `.mhtml`) with main-content extraction by a permissively licensed parser, keeping headings and tables. Decided (OQ-P8).

**Index (decided, OQ-P8: large sets, semantic search required).** Typical reference sets are large (hundreds of pages, many documents), so retrieval is hybrid from the first version:

- **Chunks** keep their location (file, page, character range, sheet range, slide and shape, email part) and their document's heading path.
- **Lexical:** SQLite FTS5 (BM25).
- **Semantic:** a small multilingual embedding model bundled with the app (an ONNX model of roughly 100 to 300 MB with a permissive license, run on CPU through ONNX Runtime in the kernel), so semantic search works offline with no endpoint. Vectors are stored in SQLite with `sqlite-vec`. An approved embedding endpoint can replace the bundled model by configuration; switching models re-embeds in the background.
- **Fusion:** reciprocal rank fusion of both result lists, then a per-document cap so one long report does not crowd out the rest.
- **Document map:** at ingest each document gets an outline (headings, page ranges, tables and figures list) built without a model; an optional model-written summary per document is generated on request, since it costs tokens. The agent sees the reference list with these outlines and drills down with `refs.search` and `refs.read`.
- **Ingest is incremental and background:** a large set is usable as soon as its first documents are indexed, with progress shown in the References panel.

**Into the model.** Retrieval through `refs.search` and `refs.read` is the default path, because sets are large. For a few short PDFs on an endpoint that supports native document blocks with citations (the Anthropic Messages API; probed on gateways), those PDFs can be sent whole, and the model's citations are mapped onto the deck's citation format. Either way the citation lands on the text run or chart series it supports.

**Always copied (decided, OQ-P8).** Adding a reference copies the file into the project's `references/` folder, content-addressed by hash, with its original path and import time recorded. A project folder is therefore self-contained and can be handed to a colleague or archived with its sources. Re-adding a changed file creates a new version; citations keep pointing at the version they were made against, and the app offers to re-check citations against the new version. Large sets make projects large; the References panel shows the size and a 'remove unused references' action (references no slide cites).

**Source lens.** Clicking a cited run opens the Sources panel at the passage with the span highlighted. The number check extracts numeric tokens from a slide's text (with unit, percent, currency, and scale normalization: "18%", "0.18", "€1.2bn") and looks for them in the cited passage; badge green (found), amber (found after normalization or rounding), red (not found), grey (no citation).

**Fibration Lab references (decided, v1).** Pointing a project at an Analyst repo lists its figures (`out/figures/**` with sidecar metadata: script, arguments, run id, target, title, axes) and its output tables. Consistent with both the always-copy rule and Analyst's principle that data stays where it is generated, only what is imported is copied: a figure (PNG and SVG with its sidecar) or a bounded table extract, never the repo or its datasets. Remote-target outputs are read through Analyst's own rendered-products path, not fetched directly. The imported item keeps the Analyst provenance as its citation. A web page or URL is not a reference type in v1 (no web access; later through an MCP connector).

## 9. Deck contract and checker

Analogue of Analyst's script contract: rules the agent is told and the kernel checks. Severity (`error`/`warn`) per rule in `.present/project.toml`; a managed configuration can pin severities.

| Rule | Check |
|---|---|
| `tpl.layout-only` | Every slide uses a layout from the template; free elements only where allowed. |
| `tpl.theme-fonts`, `tpl.theme-colors` | No font or color outside the theme (plus an allowed list). |
| `fit.overflow` | Text fits its placeholder at the placeholder's font and size, measured with the real font files (HarfBuzz shaping through `uharfbuzz`, or Pillow as a fallback), honoring the placeholder's autofit setting. |
| `fit.min-size` | No text below a minimum size after autofit shrink. |
| `density.bullets`, `density.words` | Maximum bullets per slide and words per slide (template sidecar or project config). |
| `src.coverage` | Every factual paragraph and every chart has at least one citation (factual is decided by a simple heuristic plus the agent's own tagging). |
| `src.numbers` | Number check (section 8). |
| `src.chart-stale` | A chart's stored values match its transform pipeline recomputed from the cited reference version. |
| `a11y.alt-text`, `a11y.contrast`, `a11y.reading-order` | Images have alt text, text meets WCAG AA contrast against its fill, reading order is set. |
| `struct.titles` | Every slide has a unique, non-empty title. |
| `struct.mandatory` | Mandatory slides from the template sidecar are present. |
| `img.resolution` | Images meet a minimum effective DPI at their placed size. |
| `out.opens-clean` | The exported file validates against the OOXML schemas and re-opens in `python-pptx` and in the fidelity renderer. |

Checker results appear in the Checks panel, as badges on slide thumbnails, and as tool results to the agent.

## 10. Rendering and export

**Live preview (renderer).** The UI draws slides from the deck document plus the template description: placeholders positioned from the layout, text styled from the placeholder's level styles with the theme fonts, charts drawn from data, images placed with their crop. Fast enough to update while typing. It is an approximation of PowerPoint and is labeled as such in the developer view; the checker's overflow measurement, not the preview, is authoritative.

**Fidelity render (kernel).** The actual exported `.pptx` rendered to PDF and PNG by the best available engine, in order: PowerPoint (if installed and allowed), bundled LibreOffice, system LibreOffice. Used for slide thumbnails in the grid after each accepted change (debounced), for `slide.render`, for layout thumbnails, and for PDF and image export. Without any engine, PDF and PNG export and code-mode previews are unavailable and the app says so (decided, OQ-P6). Engine selection is a setting (auto, PowerPoint, LibreOffice) so a user can force LibreOffice when PowerPoint automation is slow or unwanted; on macOS PowerPoint automation needs the user's Automation permission, requested on first use. Renders run in a single background worker per engine with a queue, since PowerPoint automation is not concurrent.

| Format | Path | Notes |
|---|---|---|
| `.pptx` | Deck document + template → `python-pptx` | Primary output. Template masters and layouts preserved; document properties, sections, notes, alt text, stable ids. |
| PDF | Fidelity render | Options: slides, slides with notes (later), handout grid (later). |
| PNG / JPEG per slide | Fidelity render | Resolution option. |
| Markdown outline | Deck document | Titles, bullets, notes, and a source list with citations. |
| Speaker script `.docx` | Deck document → `python-docx` | Slide title, notes, sources per slide. |
| `.odp` | LibreOffice conversion | Later, only with LibreOffice present. |
| HTML (self-contained) | Live preview renderer serialized | Later. Approximate fidelity; labeled. |
| `.potx` | Template + deck theme | Later. |
| Google Slides, Keynote | Not separate formats | Both import `.pptx`. |

Export presets (format, options, file naming) live in project config. Each export records the deck snapshot it came from and its checker result.

## 11. Models and enterprise endpoints

**(shared)** The `ModelClient` interface, streaming, tool-call parsing, capability probing, and the "no model-specific code" rule come from Analyst section 5.10. Additions:

| Provider kind | Wire format | Authentication |
|---|---|---|
| `anthropic` | Anthropic Messages | API key or bearer token from the environment or `.env`; `base_url` for an internal gateway that speaks Messages; optional extra headers the gateway requires |
| `openai-compatible` | Chat completions | API key, bearer token, or none; covers vLLM, Ollama, TGI, LM Studio, LiteLLM and other gateways |

**Decided (OQ-P2): first-version endpoints** are the Anthropic API directly, internal API gateways (Anthropic Messages or OpenAI-compatible; they must be reachable directly with a publicly trusted certificate, since proxies and private CAs are not supported), and open-weight models on internal GPU servers. These are exactly Analyst's two wire adapters, so the provider layer can be shared as is. Amazon Bedrock (SigV4 and the AWS credential chain), Google Vertex AI (Application Default Credentials), and Microsoft Foundry (key or Entra ID) are later additions: they are authentication back ends on the Anthropic Messages adapter and change nothing above it.

Gateway realities the adapters must handle: extra required headers; model ids that are gateway aliases rather than vendor ids (capabilities are probed, not inferred from the name); gateways that strip or reject newer request fields (unknown-field errors trigger a capability downgrade rather than a failure); and missing model-listing endpoints (manual model entry).

Features that differ by platform are probed or configured, not assumed. Relevant here:

- **Provider-hosted skills and code execution are not used.** Anthropic's server-side Agent Skills, code execution, Files API, and web tools exist only on Anthropic's own API (and not on Bedrock, Vertex, or Azure-hosted Foundry); gateways and open-weight servers have none of them. Running skills in the app (section 12) is what makes the same behavior possible on every endpoint.
- **Document blocks with citations, prompt caching, and structured outputs** are used when the endpoint supports them, as probed; the loop works without them.

**Capability baseline (decided, OQ-P13):** native tool calling, streaming, a context window of at least 128k tokens, and vision (image input). Checked by the capability probe on first use. Native tool calling is required: a model that fails the tool-calling probe is rejected with a clear error. A model below the context or vision baseline can still be selected but runs as a flagged, degraded tier (no self-review without vision, aggressive compaction below 128k), which the evaluation suite does not certify; a managed configuration can forbid it. Certification targets for the evaluation suite: Claude through the Anthropic API or your internal gateway, and at least one open-weight model on your internal servers that meets the baseline.

**Locality and audit.** The chat header names the provider, region, and whether the endpoint is internal or external. Every request writes an audit record locally: time, provider, model, token counts, and the list of reference excerpts and images included (by reference id and location, not content). The audit log can be exported for compliance review.

## 12. Skills

**Format.** Anthropic Agent Skills: a folder with `SKILL.md` (front matter `name`, `description`; markdown body) plus optional resources (templates, examples, brand guides) and scripts. This is the format Claude users already author and share, and Analyst can adopt it too (OQ-P11).

**Execution by the app.** At the start of a conversation the agent sees each available skill's name and description. It loads a skill's body with `skill.load` and its resources with `skill.read_resource` when relevant; the user can pin or disable skills per project. Scripts run only through `skill.run_script`, under policy (`ask` by default; `deny` by configuration), in the code sandbox (section 12.1) with the skill folder read-only and outputs importable as assets.

**Sources.** Built-in skills (storyline writing, chart selection, executive summary, the deck contract explained), `~/.present/skills/`, project `.present/skills/`, and a managed organization library that users cannot modify.

### 12.1 Code sandbox (decided, OQ-P7)

One sandbox for all code the app runs: code-mode builds, generated slides, and skill scripts. No general shell or REPL exists.

| Resource | Default |
|---|---|
| Interpreter and packages | The bundled Python runtime with a fixed, pinned package set shipped in the installer: `python-pptx`, `present_sdk`, `openpyxl`, `python-docx`, `Pillow`, `numpy`, `pandas`, `matplotlib`. No package installation at run time; the set is versioned with the app and listed in the about dialog for audit. |
| Filesystem read | The runtime, the deck SDK, the project's `template/`, `assets/`, and `deck/`, the running skill's folder, and reference extracts the agent passes explicitly. Home directory and the rest of the disk hidden. |
| Filesystem write | A per-run scratch directory and the build output path only. |
| Network | None, on every platform. |
| Resources | Wall-clock timeout (default 60 s per build), memory cap, process count. |
| Environment | Minimal allowlist; no credentials ever. |

Per platform, without admin rights or extra installs:

- **macOS:** `sandbox-exec` with a generated profile (Analyst's profile adapted).
- **Windows:** an AppContainer process (no network capabilities; access granted only to the folders above through ACLs on a per-run directory) inside a job object for CPU, memory, process, and UI limits.
- **Linux (phase 4):** `bubblewrap`, as in Analyst.

Where the isolation primitive is unavailable (for example AppContainer blocked by policy), runs fall back to resource limits only and every affected card is badged "unsandboxed"; a managed configuration can turn that fallback into a refusal.

## 13. Policy, gating, and checkpoints

**(shared)** Analyst sections 5.8 and 5.9, with slides as the unit:

```toml
[policy]
preset   = "review"          # optional: review | auto | read-only
edits    = "review"          # review | auto-new-slides | auto | deny
skills_scripts = "ask"       # ask | auto | deny
export   = "auto"
locked_slides = []           # also set from the UI
max_steps = 60
```

- **Presets.** `preset` is the same key as in the analysis workbench and expands to the detailed keys, which still override: slide and element edits are "edits", deck builds (`code`) and skill scripts (`skills_scripts`) are "runs". `review`: edits reviewed, builds and skill scripts start on their own. `auto`: edits inside the declared scope applied, outside it asked. `read-only`: no edit is applied and the switch to code generation mode is refused; builds and skill scripts still run. Without `preset` the defaults above apply. There is no `mcp` key; a project file that still has one loads and the key is ignored.
- **Review** pauses on each slide proposal; the card shows before/after thumbnails and a text diff, with accept, reject, or "accept with my edits".
- **Conflict detection** by content hash per slide, as in Analyst.
- **Locks** on slides and on individual elements; proposals touching them are denied.
- **Snapshots** (decided, OQ-P10) before every turn that can change the deck, at every mode switch, and at every export. A snapshot is a manifest (slide documents or build script and modules, deck metadata, the asset and reference hashes they use) written to `.present/history/` as content-addressed blobs; references and assets are already content-addressed, so they are never duplicated. One-click revert of a turn, restore of any snapshot as a new snapshot (history is append-only), and a history view with thumbnails from the stored fidelity renders. Pruning by age and count keeps export and mode-switch snapshots. No git dependency and no git integration; a user may put the project folder under git, and `.present/history/` is then best ignored.

## 14. User interface

The layout keeps Analyst's shape (project tabs on top, sidebar left, work area center, chat right, dock bottom) and its theme tokens, density, and keymap conventions, so moving between the two apps is seamless.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ [Q3 review · Corp 2026 ●] [Board pre-read] [+]    Opus 5.5 · Gateway ▾ ⚙ │
├──────────┬───────────────────────────────────────┬───────────────────────┤
│ Slides   │  ┌─────────────────────────────────┐  │ Agent chat            │
│  1 ▣ ✓   │  │ Margin recovered to 18% on      │  │ mode: Build ▾         │
│  2 ▣ ✓   │  │ pricing actions                 │  │                       │
│  3 ▣ ⚠   │  │ • Gross margin up 3.1 pts ① ●   │  │ [outline card]        │
│  4 ▣ ✓   │  │   – Two thirds from EMEA ②      │  │ [slide 4 ◧→◨ ✓ ✗]     │
│ Outline  │  └─────────────────────────────────┘  │ [check card]          │
│ Refs     │  Layout: Title and Content ▾  🔒      │                       │
│ Assets   ├───────────────────────────────────────┤                       │
│ Skills   │ Sources | Checks | Notes | Exports    │                       │
└──────────┴───────────────────────────────────────┴───────────────────────┘
```

| Panel | Interactions |
|---|---|
| Project tabs | Switch, create, close; each shows template and model. |
| Slides sidebar | Thumbnail rail with check badges and lock icons; drag to reorder; sections. |
| Outline view | Editable table of slides: action title, key message, layout, sources, status. The storyline editor. |
| References | List with extraction status; open a reference in the Sources panel; drag a passage into chat. |
| Assets | Images and imported Analyst figures. |
| Skills | Available skills, active state per project. |
| Slide canvas | Live preview; click to edit text in place; layout picker; lock slide or element; edit chart, table, and diagram-primitive content in a side sheet; citation markers that open the source lens. No free move, resize, or styling (decided, OQ-P4): positioning comes from layouts and primitives, and pixel polish happens in PowerPoint. In code mode the canvas is read-only and edits go to the script. |
| Agent chat | Streaming; cards for outline, slide proposals, checks, skill script runs, exports, approvals; mode and model switch; context indicator. |
| Sources | Reference viewer at the cited passage (PDF page view, document text, sheet range, slide). |
| Checks | Diagnostics by slide and rule, click to jump. |
| Notes | Speaker notes editor for the active slide. |
| Exports | History with format, snapshot, checker status; open file or folder. |

Docking with `dockview` and undockable windows as in Analyst (a presenter-style second window showing the slide canvas large is a natural use).

## 15. Storage layout

```
~/.present/  (or %APPDATA%\Present on Windows)
  config.toml           providers, models, UI prefs
  skills/  templates/   user libraries
  .env                  provider keys (mode 0600), written by the settings dialog
  egress.jsonl          one line per network decision (destination and purpose, never content)
  runtime/              bundled Python runtime and kernel (installed by the installer)
  audit/                audit log (JSONL, rotated)
  projects/<id>/
    project-state.toml  path, layout, active model
    conversations/<id>.jsonl
    index.sqlite        reference index (FTS5 + vectors), check cache, capability records

Managed configuration (read-only, admin-owned):
  macOS:   /Library/Application Support/Present/policy.toml (or a configuration profile)
  Windows: %ProgramData%\Present\policy.toml (or registry under HKLM\Software\Policies\Present)
  Linux:   /etc/present/policy.toml

<project>/
  .present/
    project.toml        policy, contract severities, export presets, brief
    skills/
    history/            snapshots (content-addressed)
  deck/
    deck.json           mode, order, sections, metadata
    slides/<id>.json    structured mode (generated slides carry a script)
    build.py            code mode (plus any modules it imports)
  template/             template file + template.toml sidecar
  references/           copied references, content-addressed, + manifest.toml (original paths, versions)
  assets/
  exports/
```

## 16. Security and privacy

- No telemetry, no update server, no MCP servers. The app reaches the network only for configured model endpoints, package installs, and SSH targets, and only from the Rust core through the network gate (`wb-net`), which records every decision in `egress.jsonl`. Present itself uses only the first of these. The deck kernel runs with no network.
- Credentials (API keys, gateway tokens) come from the process environment or the untracked `.env` file in the home directory. Never logged, never given to skill scripts.
- Reference content leaves the machine only as excerpts in model requests; each request is recorded in the audit log.
- Managed configuration can lock: allowed providers and models, disabled features (skill scripts, specific export formats), checker severities, required templates, the data-classification footer.
- Skill scripts run sandboxed without network.
- Exports carry no hidden data from the app beyond stable ids and document properties; an option strips citations from notes.

## 17. Testing and evaluation

- **Unit and integration:** `cargo test` for the core; `pytest` for the kernel; IPC contract tests from the `protocol` crate; renderer tests with Vitest; UI tests with Playwright against a mock backend, as in Analyst.
- **Output validity, on every change:** render a corpus of deck documents against several real templates; validate every `.pptx` against the ECMA-376 schemas; re-open with `python-pptx`; convert with LibreOffice; on a Windows CI runner with PowerPoint (if available) open and save each file and fail on repair prompts.
- **Agent evaluations, model agnostic:** deck tasks (build from a report, build from a spreadsheet, restyle onto a new template, shorten a deck to N slides, fix checker failures) with graders that inspect only the output: opens clean, checker passes, citation coverage, number-check pass rate, slide count and layout use as asked, no edits to locked slides. Scorecard per configured model and provider, as in Analyst.

## 18. Repository structure

The normative layout, including the shared-to-be `wb-*` crates and `packages/wb-ui`, is in [BUILD-SPEC.md](BUILD-SPEC.md) section 3.

**Shared code (decided, OQ-P11).** Code common to both apps lives in a third repository (working name `agent-workbench-common`) and is consumed by Present and Analyst as git dependencies pinned to tags:

| Package | Contents | Extraction order |
|---|---|---|
| `wb-providers` (crate) | `ModelClient`, Anthropic Messages and OpenAI-compatible adapters, SSE parsing, capability probing, gateway quirks, audit hook; all requests built by the network gate (`wb-net`) | Phase 0 of Present (first) |
| `wb-policy` (crate) | Policy evaluation, ask/deny/review states, scope declaration, protected items | Phase 0 (first) |
| `wb-transcript` (crate) | Turn and event types, JSONL store, compaction and pinned facts | Phase 0 |
| `wb-skills` (crate) | Agent Skills loader (`SKILL.md`, resources, script manifests), progressive disclosure tools, Analyst legacy front matter (`triggers`, `paths`, `always`) as optional extensions | Phase 0 |
| `wb-sandbox` (crate + profiles) | macOS `sandbox-exec` profile generation, Linux `bubblewrap`, Windows AppContainer + job object | Phase 1 (Present needs Windows; Analyst gains it for its phase 4) |
| `@wb/ui` (npm package) | Theme tokens (Analyst's `theme.css`), chat panel and card components, dialogs, toasts, palette, dockview setup, IPC `Backend` interface and mock helpers | Phase 1 |

Each app keeps its own `protocol` crate for domain types; shared types live in the packages above and are re-exported. Extraction is done by moving code out of Analyst with its history, then switching Analyst to the dependency in the same change, so neither app ever runs on a fork. Analyst adopts `SKILL.md` through `wb-skills` while still reading its existing skills during a transition.
