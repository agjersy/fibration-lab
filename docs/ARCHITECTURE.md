# Personal Present — Application Design and Architecture

*Status: draft for workshop (2026-11-15). Not yet a build spec: decisions marked "Proposed" or "Open" are to be settled in [OPEN-QUESTIONS.md](OPEN-QUESTIONS.md) first. Companion documents: [VISION.md](VISION.md). Reference architecture: Personal Analyst `docs/ARCHITECTURE.md`; sections marked **(shared)** reuse its design and are only summarized here.*

## 1. Decision summary

| # | Decision | Resolution | Status |
|---|---|---|---|
| P1 | Desktop shell | Tauri 2, Rust core, React renderer in the OS webview, as in Analyst (D1) | Proposed (shared) |
| P2 | Platforms | Windows and macOS first-class from phase 1 (that is where PowerPoint users are); Linux built from the same code and supported in phase 4 | Decided (OQ-P1) |
| P3 | Source of truth for a deck | Two project modes. Structured (default): a typed deck document (JSON, one file per slide) bound to the template's layouts, with native process, timeline, matrix, and hierarchy primitives and optional script-generated slides, rendered deterministically to `.pptx`. Code generation (switchable): the deck is a sandboxed `python-pptx` build script using a shared deck SDK. Structured → code is a lossless eject; code → structured is a revert or a best-effort import | Decided (OQ-P3) |
| P4 | `.pptx` writer | `python-pptx` in an app-owned, bundled Python runtime (the "deck kernel"), driven over JSON-RPC like Analyst's kernel | Proposed |
| P5 | Preview and PDF/PNG export | Two paths: a live HTML/SVG preview drawn from the deck document in the renderer, and a fidelity render of the actual `.pptx` through LibreOffice headless (bundled or system) or PowerPoint itself when installed | Open, OQ-P6 |
| P6 | Model access | Analyst's `ModelClient` with its two wire adapters (Anthropic Messages, OpenAI-compatible). First version targets the Anthropic API, internal gateways speaking either format, and open-weight models on internal servers. Cloud-platform auth back ends (Bedrock, Vertex, Foundry) are a later addition behind the same interface | Decided (OQ-P2) |
| P7 | Skills | Anthropic Agent Skills format (`SKILL.md` + resources + scripts), discovered, loaded, and executed by the app, never by the provider, so skills work identically on every provider | Proposed, OQ-P7 |
| P8 | References | Local extraction and SQLite FTS5 index; search/read tools for the agent; per-element citations in the deck document. Native PDF document blocks with citations used when the provider supports them and the set is small | Proposed |
| P9 | Edit gating | Analyst's policy engine applied per slide: review (default), auto-new-slides, auto; locked slides and elements; scope declaration | Proposed (shared) |
| P10 | Checkpoints | Content-addressed snapshot store inside the project (no dependency on a system `git`) | Proposed, OQ-P10 |
| P11 | Code execution | Required: code generation mode, generated slides, and skill scripts all run Python in a local sandbox with no network, approved per policy. No general shell or REPL tool | Decided by OQ-P3; sandbox details OQ-P7 |
| P12 | Enterprise deployment | Built for you first but enterprise-ready from phase 0: offline bundling with no runtime downloads, system proxy and trust store, local audit log, no telemetry. Managed configuration and organization libraries ship in phase 3 | Decided (OQ-P2) |
| P13 | Persistence | SQLite for indexes, JSONL for transcripts, TOML for config, as in Analyst (D13) | Proposed (shared) |
| P14 | Code sharing with Analyst | Common crates (`protocol` patterns, providers, policy, transcript, skills, MCP) extracted into a shared workspace | Open, OQ-P11 |
| P15 | Name | "Personal Present": binary `present`, config `~/.present`, bundle id `dev.personal-present.app` | Proposed, OQ-P12 |

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
    TR[Tool registry + MCP client]
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
  MCP[(MCP servers)]
  Files[(Project folder\nreferences on disk)]
  AN[(Personal Analyst repo)]

  UI <-- typed IPC --> Core
  AL --> MP --> LLM
  AL --> TR --> MCP
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
| TLS and proxy | `rustls` with the OS trust store (`rustls-platform-verifier`), system proxy settings and PAC, optional client certificates | Analyst assumes a personal network. |
| CodeMirror editor | Included for code generation mode, generated slides, and skills, with per-hunk review as in Analyst | Hidden in structured mode except as a developer JSON view. |
| Terminal, SSH | Not included | The user is not assumed to be a programmer. |

Licensing rule from Analyst kept: permissive dependencies only (no GPL or AGPL). Consequences here: PyMuPDF (AGPL) is excluded, `pypdfium2` (Apache/BSD) is used for PDF; LibreOffice (MPL-2.0) is acceptable only as an unmodified separate program, and is optional.

## 4. Domain model

| Entity | Definition | Owned by |
|---|---|---|
| Workspace | Providers, models, global skills, template library, MCP servers, list of projects, managed policy. | `~/.present/` + managed config |
| Project | One deck + one template + its references + one or more conversations + layout + export history. One tab per project. | project folder |
| Template | A `.potx`/`.pptx` whose slide masters, layouts, placeholders, and theme define what the deck may use. Inspected once into a template description. | project `template/`, or the library |
| Template description | Machine-readable summary of a template: layouts with placeholder types, positions, sizes, and text styles per level; theme colors and fonts; slide size. Given to the agent as the layout catalog and to the renderer for previews. | computed, cached |
| Deck | Ordered sections and slides plus deck-level metadata (title, author, footer text, classification). | `deck/deck.json` |
| Slide | A layout reference, the content of each placeholder, optional extra elements, speaker notes, lock state, and a stable id. | `deck/slides/<id>.json` |
| Element | Text block, image, chart, table, or (later) diagram, either in a placeholder or positioned freely if the contract allows. Carries citations. | inside the slide |
| Citation | A link from a run of text or a data series to a location in a reference (page, character range, sheet and cell range, slide and shape). | inside the element |
| Reference | A source file (or an Analyst artifact) with extracted text, tables, and images, its hash, and its location. | `references/` or linked path; index in SQLite |
| Conversation, Turn, Proposal | As in Analyst. A proposal is a change set over slides. | transcript |
| Snapshot | The deck document before an agent turn, content-addressed. | `.present/history/` |
| Export | A rendered output file with its format, preset, deck snapshot id, and checker result. | `exports/` |
| Skill | Agent Skill folder. | `~/.present/skills/`, project `.present/skills/`, managed library |
| Provider / Model | Endpoint, auth back end, model id, probed capabilities. | config + keychain |
| Policy | Gating rules, locked keys from managed config. | `.present/project.toml` + managed config |

## 5. The deck document (P3)

This is the central design choice; OQ-P3 sets out the alternatives.

**Proposed:** a JSON document, validated against a published JSON Schema, in which a slide is *a layout from the template plus content for its placeholders*. The renderer turns it into `.pptx` deterministically: the same document and template always produce the same file.

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
- **Charts** are a type (bar, column, line, pie, scatter, area, combo, and their stacked variants) plus a data table plus a small set of options (axis titles, labels, legend). Rendered as native PowerPoint charts with an embedded workbook, styled from theme colors.
- **Tables** are rows and cells of text, with header row and banding flags; rendered as native tables using the template's table style.
- **Images** reference a file in the project's `assets/` (or an Analyst figure) by hash, with alt text required and a crop and fit mode into the placeholder.
- **Free elements** (outside placeholders) carry an explicit position and size in EMU and are only allowed where the deck contract allows them.
- **Stable ids** are written into the exported `.pptx` (shape names and `p:custDataLst` tags) so a later re-import can map shapes back (OQ-P5).
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
| In-app editing | Text, layout, order, locks, chart data (OQ-P4) | Edit the script; slide-level locks become "frozen" slides the build must reproduce byte-for-byte in text (checked) |
| Citations | Stored on runs and series in the document | Attached in code through the SDK (`cite(shape_or_run, "ref:...")`), written into the `.pptx` as tags and read back by the checker and source lens |
| Requires code execution | Only for generated slides and skill scripts | Always |

**Deck SDK (`present_sdk`).** A small Python library shipped with the kernel and importable by build scripts and generated slides. It gives code the same footing as the renderer: `deck = open_template()`, `add_slide(layout="Title and Content")`, placeholder access by role, theme colors and fonts by name, the diagram primitives and chart builders the renderer uses, `cite(...)`, and `alt(...)`. Scripts are expected (by the system prompt and a lint rule) to use the SDK rather than hard-coded fonts, colors, and positions, which keeps code-generated decks on-template in practice even though compliance is no longer guaranteed by construction.

**Switching.**

- **Structured → code ("eject").** The renderer has a code back end that emits an equivalent `build.py` written against the SDK: one function per slide, readable, with citations preserved. Running it reproduces the same `.pptx` (verified at switch time by comparing output). Generated slides are inlined. Lossless.
- **Code → structured.** Not generally possible: arbitrary code has no structured equivalent. The app offers (1) reverting to the structured snapshot taken before the eject, discarding code-mode changes, or (2) importing the current output `.pptx` as a new structured deck on a best-effort basis (the same importer as OQ-P5), with unmapped shapes kept as locked foreign elements. Both are explicit, previewed actions.
- Every switch is a snapshot boundary and is recorded in the transcript; the agent is told the mode in its context.

**Policy.** Code generation mode needs the sandbox (OQ-P7) and can be disabled by configuration (`policy.code_mode = "allow" | "deny"`), as an enterprise managed setting would. Builds have a time limit and no network.


## 6. Templates

**Ingestion.** The kernel opens the template with `python-pptx`, walks masters and layouts, and produces the template description: for each layout its name, the placeholders (type, index, position, size, text style per level, autofit setting), and for the theme its color scheme and major/minor fonts. Layouts whose names are unhelpful ("Layout 7") get a generated description and a thumbnail from the fidelity renderer, so the agent and the user can pick layouts by look and purpose.

**Usage hints.** A template can carry an optional sidecar `template.toml` written by the brand owner: which layouts to use for what, which to avoid, maximum bullets, approved icon and image folders, mandatory slides (title, disclaimer, closing). The sidecar is effectively a skill bound to the template and is shown to the agent with the layout catalog.

**Library.** `~/.present/templates/` plus a managed, read-only organization library path (a file share or a synced folder). Projects copy the template in, so a project folder is self-contained and reproducible.

## 7. Agent loop

**(shared)** Structure, pause-on-ask semantics, compaction, pinned facts, capability probing, and the text-protocol fallback follow Analyst section 5.7. Deck-specific parts:

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
| `slide.render` (returns a PNG of a slide for vision-capable models) | read |
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
| `mcp.<server>.<tool>` | per server |

No shell tool, no general code REPL, no file write outside the deck store, no network tool in the first version. Code runs only as a deck build, a generated slide, or a skill script.

### 7.4 Verification built into the loop

- After every accepted slide edit: the checker runs on that slide (overflow measured with the real fonts, contract rules, citation coverage, number check). Errors go back to the model before the user sees "done", at most N automatic fix iterations.
- On vision-capable models, the agent is prompted to call `slide.render` on new slides and review them against a short visual checklist (alignment, density, legibility). This is a quality step, not a gate.
- Scope declaration: in Build mode the first action on a new task lists the slide ids it will create or change; changes outside it need approval even in auto mode.

## 8. References and source tracing

**Ingestion** (kernel, local): PDF with `pypdfium2` (text with page and character offsets, embedded images); DOCX with `python-docx` (paragraphs, headings, tables); PPTX with `python-pptx` (per slide and shape, notes, and charts' data); XLSX with `openpyxl` (sheets, named ranges, cell values; formulas evaluated values only); CSV; Markdown and plain text; images (with optional captioning by a vision model on request). OCR for scanned PDFs is a later, optional component (Tesseract, Apache-2.0).

**Index.** SQLite FTS5 over chunks that keep their location (file, page, character range, sheet range, slide and shape). Embeddings are optional, only through an approved endpoint configured by the user or the organization, because many enterprise deployments have no embedding model.

**Two ways into the model.** For a small reference set on a provider with native document support, PDFs can be sent as document blocks with citations enabled (supported by the Anthropic Messages API; a gateway or open-weight server may not support it, so this is probed), and the model's citations are mapped onto the deck's citation format. Otherwise, and always for large sets, the agent uses `refs.search` and `refs.read`. In both cases the citation lands on the text run or chart series it supports.

**Linked or copied.** References are linked by absolute path with a content hash by default, with an option to copy into `references/` for a self-contained project (OQ-P8).

**Source lens.** Clicking a cited run opens the Sources panel at the passage with the span highlighted. The number check extracts numeric tokens from a slide's text (with unit, percent, currency, and scale normalization: "18%", "0.18", "€1.2bn") and looks for them in the cited passage; badge green (found), amber (found after normalization or rounding), red (not found), grey (no citation).

**Personal Analyst references.** Pointing a project at an Analyst repo exposes its `out/figures/**` with sidecar metadata (script, arguments, run id, title, axes) and its run tables through `out.read`-style previews. A figure imported into a deck keeps that provenance as its citation.

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
| `a11y.alt-text`, `a11y.contrast`, `a11y.reading-order` | Images have alt text, text meets WCAG AA contrast against its fill, reading order is set. |
| `struct.titles` | Every slide has a unique, non-empty title. |
| `struct.mandatory` | Mandatory slides from the template sidecar are present. |
| `img.resolution` | Images meet a minimum effective DPI at their placed size. |
| `out.opens-clean` | The exported file validates against the OOXML schemas and re-opens in `python-pptx` and in the fidelity renderer. |

Checker results appear in the Checks panel, as badges on slide thumbnails, and as tool results to the agent.

## 10. Rendering and export

**Live preview (renderer).** The UI draws slides from the deck document plus the template description: placeholders positioned from the layout, text styled from the placeholder's level styles with the theme fonts, charts drawn from data, images placed with their crop. Fast enough to update while typing. It is an approximation of PowerPoint and is labeled as such in the developer view; the checker's overflow measurement, not the preview, is authoritative.

**Fidelity render (kernel).** The actual exported `.pptx` rendered to PDF and PNG by the best available engine, in order: PowerPoint (if installed and allowed), bundled LibreOffice, system LibreOffice. Used for slide thumbnails in the grid after each accepted change (debounced), for `slide.render`, for layout thumbnails, and for PDF and image export. Without any engine, PDF and PNG export are unavailable and the app says so.

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
| `anthropic` | Anthropic Messages | API key or bearer token from the keychain; `base_url` for an internal gateway that speaks Messages; optional extra headers the gateway requires |
| `openai-compatible` | Chat completions | API key, bearer token, or none; covers vLLM, Ollama, TGI, LM Studio, LiteLLM and other gateways |

**Decided (OQ-P2): first-version endpoints** are the Anthropic API directly, internal API gateways (Anthropic Messages or OpenAI-compatible, typically behind a corporate proxy with a private CA), and open-weight models on internal GPU servers. These are exactly Analyst's two wire adapters, so the provider layer can be shared as is. Amazon Bedrock (SigV4 and the AWS credential chain), Google Vertex AI (Application Default Credentials), and Microsoft Foundry (key or Entra ID) are later additions: they are authentication back ends on the Anthropic Messages adapter and change nothing above it.

Gateway realities the adapters must handle: private CA certificates and system proxy settings (section 3); extra required headers; model ids that are gateway aliases rather than vendor ids (capabilities are probed, not inferred from the name); gateways that strip or reject newer request fields (unknown-field errors trigger a capability downgrade rather than a failure); and missing model-listing endpoints (manual model entry).

Features that differ by platform are probed or configured, not assumed. Relevant here:

- **Provider-hosted skills and code execution are not used.** Anthropic's server-side Agent Skills, code execution, Files API, and web tools exist only on Anthropic's own API (and not on Bedrock, Vertex, or Azure-hosted Foundry); gateways and open-weight servers have none of them. Running skills in the app (section 12) is what makes the same behavior possible on every endpoint.
- **Document blocks with citations, prompt caching, vision, and structured outputs** are used when the endpoint supports them, as probed; the loop works without them.

**Capability baseline:** native tool calling, streaming, a context window of at least 128k tokens (references and the layout catalog are large), and vision recommended (self-review and image references degrade gracefully without it).

**Locality and audit.** The chat header names the provider, region, and whether the endpoint is internal or external. Every request writes an audit record locally: time, provider, model, token counts, and the list of reference excerpts and images included (by reference id and location, not content). The audit log can be exported for compliance review.

## 12. Skills

**Format.** Anthropic Agent Skills: a folder with `SKILL.md` (front matter `name`, `description`; markdown body) plus optional resources (templates, examples, brand guides) and scripts. This is the format Claude users already author and share, and Analyst can adopt it too (OQ-P11).

**Execution by the app.** At the start of a conversation the agent sees each available skill's name and description. It loads a skill's body with `skill.load` and its resources with `skill.read_resource` when relevant; the user can pin or disable skills per project. Scripts run only through `skill.run_script`, under policy (`ask` by default), in a sandbox where the OS allows (Analyst's `sandbox-exec` profile on macOS; `bubblewrap` on Linux; a restricted token and job object on Windows, badged as weaker), with no network and write access limited to a scratch directory whose outputs can be imported as assets.

**Sources.** Built-in skills (storyline writing, chart selection, executive summary, the deck contract explained), `~/.present/skills/`, project `.present/skills/`, and a managed organization library that users cannot modify.

## 13. Policy, gating, and checkpoints

**(shared)** Analyst sections 5.8 and 5.9, with slides as the unit:

```toml
[policy]
edits    = "review"          # review | auto-new-slides | auto
skills_scripts = "ask"       # ask | auto | deny
mcp      = "ask"
export   = "auto"
locked_slides = []           # also set from the UI
max_steps = 60
```

- **Review** pauses on each slide proposal; the card shows before/after thumbnails and a text diff, with accept, reject, or "accept with my edits".
- **Conflict detection** by content hash per slide, as in Analyst.
- **Locks** on slides and on individual elements; proposals touching them are denied.
- **Snapshots** before every turn that can change the deck, stored content-addressed in `.present/history/`, with one-click revert of a turn. No system `git` required; a project folder can still be put under git by the user.

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
| Slide canvas | Live preview; click to edit text in place; layout picker; lock; citation markers that open the source lens. |
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
  mcp.json
  runtime/              bundled Python runtime and kernel (installed by the installer)
  audit/                audit log (JSONL, rotated)
  projects/<id>/
    project-state.toml  path, layout, active model
    conversations/<id>.jsonl
    index.sqlite        reference index, check cache, capability records

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
  references/           copied references (optional) + links.toml
  assets/
  exports/
```

## 16. Security and privacy

- No telemetry. Network calls only to configured model providers, configured MCP servers, and (if enabled) the update server. Updates can be disabled by managed config.
- Credentials (API keys, gateway tokens) in the OS keychain. Never logged, never given to skill scripts or MCP servers unless configured.
- Reference content leaves the machine only as excerpts in model requests; each request is recorded in the audit log.
- Managed configuration can lock: allowed providers and models, disabled features (skill scripts, MCP, specific export formats), checker severities, required templates, the data-classification footer.
- Skill scripts run sandboxed without network; MCP servers go through the policy engine.
- Exports carry no hidden data from the app beyond stable ids and document properties; an option strips citations from notes.

## 17. Testing and evaluation

- **Unit and integration:** `cargo test` for the core; `pytest` for the kernel; IPC contract tests from the `protocol` crate; renderer tests with Vitest; UI tests with Playwright against a mock backend, as in Analyst.
- **Output validity, on every change:** render a corpus of deck documents against several real templates; validate every `.pptx` against the ECMA-376 schemas; re-open with `python-pptx`; convert with LibreOffice; on a Windows CI runner with PowerPoint (if available) open and save each file and fail on repair prompts.
- **Agent evaluations, model agnostic:** deck tasks (build from a report, build from a spreadsheet, restyle onto a new template, shorten a deck to N slides, fix checker failures) with graders that inspect only the output: opens clean, checker passes, citation coverage, number-check pass rate, slide count and layout use as asked, no edits to locked slides. Scorecard per configured model and provider, as in Analyst.

## 18. Repository structure (proposed)

```
apps/desktop/
  src-tauri/          Tauri shell (thin glue over crates/core)
  src/                React renderer
crates/protocol/      IPC and kernel types; TypeScript bindings via ts-rs
crates/core/          projects, agent loop, policy, providers, skills, references, deck store, audit
crates/cli/           headless harness (`present`) for phase 0 and evals
python/kernel/        present_kernel: template inspector, renderer (pptx and code back ends), extractors, checker, fidelity renderer, sandboxed build runner
python/sdk/           present_sdk: deck SDK used by the renderer, build scripts, and generated slides
schemas/              deck document JSON Schema, template description schema
evals/                deck tasks, graders, templates, scorecard runner
docs/
```

Whether `protocol`, provider, policy, transcript, skills, and MCP code is shared with Analyst through a common workspace is OQ-P11.
