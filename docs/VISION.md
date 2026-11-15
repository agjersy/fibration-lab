# Personal Present — Product Vision

*Working name. Status: draft for workshop (2026-11-15). Companion documents: [ARCHITECTURE.md](ARCHITECTURE.md), [OPEN-QUESTIONS.md](OPEN-QUESTIONS.md). Sister product: Personal Analyst (`agjersy/personal-analyst`), whose architecture, conventions, and visual style this product follows.*

## 1. One line

A desktop workbench in which an AI agent builds presentations from your brief and your reference materials, on your organization's template, under your supervision, and produces native, fully editable PowerPoint files whose every claim you can trace back to a source.

## 2. Where this comes from

Building a deck with a chat assistant today goes one of three ways, and each breaks somewhere:

| Approach | What actually happens |
|---|---|
| Ask a chat model for an outline, then build slides by hand | The model does the easy part (bullets) and you still do the slow part (layout, template, charts, consistency). Facts drift between the outline and the slides with no link back to the source. |
| Let a model generate the `.pptx` with code | The file opens, but it does not use the corporate template's layouts, text is placed in free text boxes rather than placeholders, charts are pictures, fonts and colors are off-brand, and text overflows. Editing it in PowerPoint is harder than starting over. |
| Use a hosted "AI slides" product | The output lives in someone else's format and cloud, the materials you upload leave your organization, and the model behind it is not one your organization approved. |

Common pain points underneath:

| Pain point | Consequence |
|---|---|
| Unverifiable content | A number on slide 7 cannot be traced to the report it came from. Reviewing a generated deck means re-reading every source. |
| Template non-compliance | Brand teams reject decks; users spend the time saved on reformatting. |
| Not really editable | Final edits always happen in PowerPoint. Output that is not built from native placeholders, charts, and tables is a dead end. |
| Enterprise constraints | Models must be the ones the organization hosts or contracts (often Claude behind an internal API gateway, or open-weight models on internal servers). Materials may not leave approved boundaries. Software must install and run without internet access. |
| Fragmented surfaces | Source documents, the chat, the deck, the template, and the export tool are separate windows. |

## 3. Vision

**Personal Present is a source-grounded, template-native deck builder.** Its job is not to make slides faster at any cost. Its job is to make the deck the agent produces cheap to check, correct by construction against the template, and ready for final editing in PowerPoint.

### Principles

1. **Show, don't ask for trust.** Every agent change is shown as a slide-level before/after. Every factual statement and number on a slide carries a link to the passage in the reference material it came from. Where a check can be automated (does this number appear in the cited source? does this text fit its placeholder?), the app runs it and shows the result. *(Analyst: "show, don't ask for trust".)*
2. **You own the deck.** Agent changes are proposals until accepted, at a strictness configured per project. Slides and elements can be locked. Every agent turn is checkpointed so any change is one click from undone. The agent declares which slides it will touch before touching them. *(Analyst: "you own the code".)*
3. **Native, not pictures.** The product of a session is a `.pptx` that a PowerPoint user cannot tell from a hand-built one: real slide layouts and placeholders from the template, native charts with editable data, native tables, speaker notes, alt text, sections. Images are used only for what is genuinely an image. *(Analyst: "tools, not transcripts".)*
4. **The template is the contract.** The organization's template defines what layouts, fonts, colors, and sizes are allowed. The agent is held to it and a checker enforces it automatically, in the same way Personal Analyst enforces its script contract.
5. **Model agnostic, enterprise ready.** Claude through the Anthropic API or an internal gateway; open-weight models on internal servers through any OpenAI-compatible API; cloud platforms (Bedrock, Vertex, Foundry) later through the same interface. No model-specific code paths. No telemetry, no cloud dependency of the application itself, installable and usable offline. The app always tells you which provider your materials are sent to.
6. **Materials stay put.** Reference files are read where they are. Only the excerpts the agent needs are sent to the model, and the app keeps a local record of what was sent where.
7. **One family with Personal Analyst.** Same shell, same layout logic, same look, same agent controls, same skill format. Analysis results (figures with provenance, tables, equations) flow from Analyst into Present without losing their provenance.

## 4. Who it is for

- **Primary:** a professional who regularly turns material (reports, analyses, spreadsheets, earlier decks) into presentations on a mandated corporate template, and finishes them in PowerPoint. Comfortable with PowerPoint; not necessarily a programmer.
- **Secondary (phase 3):** the same user's organization: IT deploys the app with a managed configuration (approved model endpoints, locked policies, shared skills and templates). The app is enterprise-ready from the start (offline install, proxy and CA, audit log) so this needs no rework.
- **Also:** you, as a Personal Analyst user turning analysis results into a talk or report-out.
- **Not for:** freeform graphic design, animation-heavy keynote production, or real-time multi-user co-editing.

> This is a deliberate widening relative to Personal Analyst, whose user is a single programmer. It is the main source of new requirements (Windows, managed deployment, non-technical UI). See OQ-P1 and OQ-P2.

## 5. What it is and is not

**It is:** a slide workbench with a live preview grid, an agent tuned for storyline-first deck building, a reference library with search and source tracing, a template inspector, a deck checker, and an export pipeline, in one window with projects as tabs.

**It is not:** a PowerPoint replacement (PowerPoint remains the place for final pixel-level editing), a hosted service, a design tool, or a document management system.

## 6. Core experiences

These walkthroughs are the acceptance bar for the first usable version.

**Start a project.** Create a project, choose a template (`.potx` or `.pptx`) from the organization library or a file, and add references: drop in PDFs, Word documents, spreadsheets, earlier decks, images, or a Personal Analyst repo. The app indexes them locally and shows each with its page count and extraction status. Choose a model from the configured providers; the header shows "Claude Opus 5.5 · corporate gateway (internal)".

**Brief and storyline.** "A 12-slide quarterly review for the leadership team from the Q3 report and the pipeline spreadsheet; lead with the margin recovery." In Outline mode the agent proposes a storyline: one row per slide with an action title (the sentence the slide proves), the key evidence, the intended layout, and the sources. You reorder two rows, cut one, and rewrite a title inline. Nothing has been rendered yet; the storyline is the cheapest place to steer.

**Build.** You approve the outline and switch to Build. The agent produces slides one at a time on the template's layouts, filling real placeholders. The revenue table from the spreadsheet becomes a native chart with its data embedded. Each slide appears in the grid as it is proposed, with a diff card in chat. Checks run as each slide lands: one slide has text that overflows its placeholder at the template's font, and the agent shortens it before you see it as done. The agent looks at the rendered image of its own slides (on vision-capable models) and fixes a cramped layout.

**Verify.** You click a number on slide 4. The Sources panel opens the Q3 report at page 11 with the passage highlighted; the number badge is green because the value appears in the cited passage. One bullet on slide 9 has a grey "unsourced" badge; you ask the agent to source it or cut it. The Checks panel is clean: template fonts and colors only, no overflow, alt text on every image, contrast passes.

**Edit.** You change a word directly on the slide canvas, swap slide 6 to a two-column layout from the layout picker, and lock slide 1 so the agent cannot change it. The agent's next turn sees your edits.

**Export.** Export to `.pptx` and open it in PowerPoint: the slides use the template's layouts, placeholders hold the text, charts open their data in Excel, speaker notes carry the talking points and source references. Also export a PDF for the pre-read and a Markdown outline for the email. Exports are named and versioned in the project.

**Switch.** A second project tab holds a different deck with its own template, references, conversation, and layout. Switching restores everything.

## 7. Feature pillars and priority

| Pillar | First version | Later |
|---|---|---|
| Agent loop for decks | Outline, build, polish cycle. Slide-level proposals with review gating. Scope declaration. Deck contract enforcement. Self-review on rendered slides for vision-capable models. No model-specific code paths. | Multi-model roles (writer, reviewer). Parallel slide generation. |
| Template engine | Import `.potx`/`.pptx` templates; inspect layouts, placeholders, theme fonts and colors; layout catalog given to the agent. Organization template library. | Template authoring helpers. Multiple masters per deck. |
| Deck model and rendering | Structured deck document as the source of truth, rendered deterministically to native `.pptx`: placeholders, text, bullets, images, native charts, native tables, notes, sections, alt text. | Native SmartArt-like diagrams, native equations (OMML), animations and transitions. |
| References | Ingest PDF, DOCX, PPTX, XLSX/CSV, images, Markdown and text. Local full-text index. Search and read tools for the agent. Citations stored per text element. | OCR for scanned PDFs. Semantic search with an approved embedding endpoint. URLs and SharePoint/Drive connectors through MCP. |
| Verification | Slide diffs. Source lens (click a claim, see the passage). Number check. Deck checker (overflow, fonts, colors, contrast, alt text, density). | Fidelity check against a real PowerPoint render. Reviewer-model pass. |
| Editing | Reorder, duplicate, delete slides. Edit text in place. Change layout. Lock slides and elements. Edit chart data. Speaker notes editor. | Freeform shape editing. Re-import edits made in PowerPoint. |
| Export | `.pptx` (primary), PDF, PNG per slide, Markdown outline, speaker notes as `.docx`. | `.odp`, self-contained HTML, `.potx` from a deck, handouts. |
| Skills | Agent Skills (`SKILL.md`) loaded and executed by the app itself, so they work identically with every endpoint, including gateways and open-weight servers. Organization skill library (read-only, managed). | Skill authoring helpers. Skill evaluation. |
| Models | Anthropic API, internal gateways (Anthropic Messages or OpenAI-compatible), open-weight models on internal servers. Keys and tokens in the OS keychain. Per-conversation model choice. Capabilities probed. | Amazon Bedrock, Google Vertex AI, Microsoft Foundry. Cost and token budgets per project. |
| Enterprise | Offline installers (MSI, PKG/DMG) with no runtime downloads. Proxy and custom CA support. Local audit log of what was sent to which provider. No telemetry. | Managed configuration with locked keys and organization template and skill libraries (phase 3). Sensitivity labels. Distribution over a file share or MCP. |
| Analyst interop | Import figures, tables, and equations from a Personal Analyst repo with their run provenance. | Live links: re-run in Analyst, refresh in Present. |

## 8. Success criteria

- **Opens clean.** Every exported `.pptx` opens in PowerPoint (Windows and macOS) without a repair prompt, and in LibreOffice and Keynote. Checked by the evaluation suite on every change.
- **Template-native.** In exported decks, all text sits in template placeholders or in elements the template contract allows, and no font or color outside the template theme is used. Checked automatically.
- **Time to verify.** For any number or factual claim, its source passage is one click away, and a number that does not appear in its cited source is flagged automatically.
- **No unwanted edits.** No slide changes without consent at the configured level, and every agent turn is revertible in one action.
- **Editable in PowerPoint.** Charts are native charts with editable data, tables are native tables, and a user can restyle the deck by switching the template theme in PowerPoint.
- **Model freedom.** The full loop works with any model meeting the capability baseline, verified by a model-agnostic evaluation suite against Claude (directly or through a gateway) and at least one open-weight model on an internal server, with no model-specific code.
- **Enterprise deployable.** Installs and runs on a machine with no internet access and only an internal model endpoint reachable.

## 9. Non-goals for the first version

Animations and transitions. Embedded video and audio. Real-time collaboration. Cloud sync. Mobile. Freeform drawing. A hosted web version. Generating images with image models (possible later through MCP).

## 10. Roadmap (draft)

| Phase | Goal | Exit criterion |
|---|---|---|
| 0. Headless core | Deck schema, template ingestion, deterministic `.pptx` renderer, deck checker, reference ingestion and search, agent loop as a CLI, provider adapters for the Anthropic API, gateways, and OpenAI-compatible servers (shared with Analyst), evaluation suite. | Three representative deck tasks complete end to end with two models from different providers on two templates; every output opens without repair in PowerPoint and passes the checker. |
| 1. Workbench | Tauri shell: project tabs, slide grid and canvas, outline view, chat with cards, references panel, checks panel, exports. Windows and macOS. | You can build and export a real deck for work without leaving the app until the final PowerPoint polish. |
| 2. Verification and editing | Source lens and number check, in-place editing, locks, re-import of PowerPoint edits (if OQ-P5 says so), fidelity check. | The walkthroughs in section 6 pass. |
| 3. Enterprise and extension | Managed configuration, organization libraries for templates and skills, MCP connectors, Analyst interop, more export formats. | IT can deploy a locked configuration offline; an Analyst figure travels into a deck with provenance. |
| 4. Polish | Performance, accessibility, Linux packaging, keyboard-first workflows. | Daily-driver quality. |
