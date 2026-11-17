# Protocol and Integration Contract

*Status: phase 0 baseline, protocol version 1. Normative for `python/kernel`, `python/sdk`, `crates/*`, `schemas/`, and `evals/`. Changes require bumping `protocol_version` (kernel) or `schema_version` (deck document) and updating every side in the same commit. Companion documents: [ARCHITECTURE.md](ARCHITECTURE.md) explains why; [IPC.md](IPC.md) is the desktop boundary; [BUILD-SPEC.md](BUILD-SPEC.md) is the build order.*

This document fixes every boundary between components so the deck kernel, the deck SDK, the Rust core, the CLI, and the evaluation suite can be built independently and fit together. Where a part is the same as Personal Analyst's PROTOCOL.md, this document says so and lists only the differences.

Conventions: types are written as TypeScript; `?` marks optional; all paths crossing a boundary are relative to the project root with forward slashes unless the field name ends in `_abs`; times are RFC 3339 UTC; lengths are EMU (914400 per inch, 12700 per point) unless stated; hashes are lowercase hex SHA-256.

## 1. Deck kernel JSON-RPC

### 1.1 Transport and framing

- Launched by the core as `<runtime_python> -I -m present_kernel --project <project_abs> --home <present_home_abs> [--engine auto|powerpoint|libreoffice|none] [--sandbox auto|native|limits]`.
- `<runtime_python>` resolution: `PRESENT_RUNTIME_PYTHON`, then `runtime_python` in `config.toml`, then the bundled runtime under `<home>/runtime/` (installed builds), then `python/kernel/.venv` in a development checkout.
- Framing is identical to Analyst PROTOCOL 1.1: newline-delimited JSON-RPC 2.0 on stdio, UTF-8, stderr for logs only, kernel may answer out of order, notifications only from kernel to core, first request `kernel.hello`, last `kernel.shutdown`.
- One kernel per open project. Long operations (ingest, render, build) report progress with `task.progress` notifications carrying the request id.

### 1.2 Errors

Standard JSON-RPC codes plus:

| code | meaning |
|---|---|
| 2000 | not found (slide, reference, layout, snapshot) |
| 2001 | path escapes the project root |
| 2002 | schema validation failed (`data.errors: {pointer: string, message: string}[]`) |
| 2003 | template unreadable or unsupported |
| 2004 | render failed (`data.element_id?` when attributable) |
| 2005 | build script failed (`data.stdout`, `data.stderr`, `data.exit_code`) |
| 2006 | timeout |
| 2007 | sandbox requested but unavailable |
| 2008 | fidelity engine unavailable |
| 2009 | unsupported reference format |
| 2010 | transform failed (`data.step_index`) |

`error.data` always has `{detail: string}`.

### 1.3 Methods

```ts
// ---- lifecycle ----
"kernel.hello":    { core_version: string, protocol_version: 1 }
  -> { kernel_version: string, protocol_version: 1, schema_version: 1, os: "windows"|"macos"|"linux",
       python: string, packages: Record<string, string>,          // the fixed bundled set with versions
       sandbox: SandboxInfo, engines: EngineInfo[], embedding: EmbeddingInfo }
"kernel.shutdown": {} -> null

interface SandboxInfo { kind: "sandbox-exec" | "appcontainer" | "bwrap" | "limits"; available: boolean; detail: string }
interface EngineInfo  { kind: "powerpoint" | "libreoffice"; available: boolean; version?: string; path_abs?: string; detail: string }
interface EmbeddingInfo { kind: "bundled" | "endpoint" | "none"; model: string; dims: number }

// ---- templates ----
"template.inspect":    { path: string }                          -> TemplateDescription         // section 3
"template.thumbnails": { path: string, dpi?: number }            -> { layout: string, png: string }[]   // png = project-relative path under .present/cache/
"template.sidecar":    { path: string }                          -> TemplateSidecar | null      // parsed template.toml, section 6.4

// ---- deck (structured mode) ----
"deck.validate": { deck?: string }                                -> Diagnostic[]   // JSON Schema + references to layouts, assets, refs
"deck.render":   { out: string, snapshot_id: string, slides?: string[] }
  -> { out: string, sha256: string, element_map: ElementMapEntry[], warnings: Diagnostic[] }
"deck.eject":    { out_dir: string }                              -> { script: string, verified: boolean, diff?: string }   // section 5.4
"slide.generate":{ slide_id: string, timeout_s?: number }         -> { out: string, stdout: string, stderr: string }        // runs a generated slide's script

// ---- deck (code mode) ----
"deck.build":    { script: string, out: string, snapshot_id: string, timeout_s?: number }
  -> { out: string, sha256: string, element_map: ElementMapEntry[], stdout: string, stderr: string, warnings: Diagnostic[] }

interface ElementMapEntry { slide_id: string; element_id: string; slide_index: number; shape_id: number; kind: string }

// ---- rendering ----
"render.fidelity": { pptx: string, format: "png"|"pdf", slides?: number[], dpi?: number, engine?: "auto"|"powerpoint"|"libreoffice", out_dir: string }
  -> { engine: "powerpoint"|"libreoffice", files: string[] }
// With no fidelity engine, slide images for slide.render come from the renderer's HTML preview (IPC.md section 5), not the kernel.

// ---- checker ----
"check.run": { pptx: string, deck?: string, slides?: string[], rules?: string[] } -> Diagnostic[]     // section 7
"text.measure": { font_family: string, size_pt: number, bold?: boolean, italic?: boolean, paragraphs: string[],
                  box: { cx: number, cy: number }, insets?: { l: number, t: number, r: number, b: number },
                  line_spacing?: number, autofit?: "none"|"shrink"|"resize" }
  -> { fits: boolean, lines: number, height: number, shrink_to_pt?: number, font_found: boolean }

// ---- references ----
"refs.ingest":  { path: string }        // path under references/ (already copied by the core)
  -> { ref_id: string, kind: RefKind, title: string, pages?: number, outline: OutlineNode[],
       chunks: Chunk[], tables: TableRef[], images: ImageRef[], children: string[], warnings: Diagnostic[] }
"refs.read":    { ref_id: string, loc: string, max_chars?: number }   -> { text: string, loc: string, truncated: boolean }
"refs.table":   { ref_id: string, loc: string }                       -> Table
"refs.page_image": { ref_id: string, page: number, dpi?: number }     -> { png: string }
"embed.texts":  { texts: string[] }                                   -> { vectors: number[][] }    // bundled model; the core calls an endpoint itself when configured

type RefKind = "pdf"|"docx"|"pptx"|"xlsx"|"csv"|"image"|"markdown"|"text"|"eml"|"msg"|"html"|"analyst-figure"|"analyst-table"
interface OutlineNode { title: string; loc: string; children: OutlineNode[] }
interface Chunk { loc: string; heading_path: string[]; text: string }
interface TableRef { loc: string; title?: string; rows: number; cols: number }
interface ImageRef { loc: string; png: string; width: number; height: number; caption?: string }
interface Table { columns: string[]; rows: (string|number|null)[][]; loc: string }

// ---- data ----
"chart.compute": { source: ChartSource, steps: TransformStep[] } -> { table: Table, step_tables?: Table[] }   // section 2.6

// ---- exports derived from the document ----
"export.outline_md":  { deck: string, out: string, include_sources: boolean } -> { out: string }
"export.notes_docx":  { deck: string, out: string, include_sources: boolean } -> { out: string }

// ---- Personal Analyst ----
"analyst.scan":   { repo_abs: string } -> { figures: AnalystFigure[], tables: AnalystTable[] }
"analyst.import": { repo_abs: string, item: string, max_rows?: number } -> { ref_id: string }     // copies into references/ (section 4.3)

// ---- notifications (kernel -> core) ----
"task.progress": { request_id: string|number, fraction?: number, message: string }
"log":           { level: "debug"|"info"|"warn"|"error", message: string }
```

### 1.4 Determinism

**Autofit.** For placeholders whose template setting is shrink-on-overflow, the SDK measures the text at save time and writes the resulting `fontScale` and `lnSpcReduction` into the slide's `a:normAutofit`, because PowerPoint only recomputes them when text is edited. The checker uses the same measurement.

`deck.render` is a pure function of (deck document files, referenced assets, template file, `present_sdk` version): identical inputs produce byte-identical output except for the `docProps/core.xml` modified time, which is fixed to the snapshot time. The test suite asserts this.

### 1.5 Diagnostics

```ts
interface Diagnostic {
  rule: string;                       // section 7 id, or "schema", "render", "build"
  severity: "error" | "warn" | "info";
  message: string;
  slide_id?: string; element_id?: string; slide_index?: number;
  pointer?: string;                   // JSON pointer into the slide document
  fix_hint?: string;
}
```

## 2. Deck document (schema version 1)

Normative JSON Schema: `schemas/deck.schema.json` (deck) and `schemas/slide.schema.json` (slide), generated from the Rust types in `crates/protocol` with `schemars` and committed; the kernel validates with `jsonschema`. The TypeScript below is the readable form.

### 2.1 Files

```
deck/deck.json
deck/slides/<slide_id>.json        structured mode
deck/build.py (+ modules)          code mode
```

```ts
interface Deck {
  schema_version: 1;
  mode: "structured" | "code";
  title: string;
  author?: string;
  template: string;                 // "template/<file>.potx|pptx"
  slide_size?: { cx: number, cy: number };   // default from template
  sections: { title: string, slides: SlideId[] }[];   // every slide id appears exactly once, in order
  footer?: { text?: string, slide_numbers?: boolean, date?: boolean };
  classification?: string;          // rendered where the template provides a footer placeholder
  brief?: string;                   // the user's brief, for the agent's context
  properties?: Record<string, string>;   // custom document properties
}
type SlideId = string;              // ^s-[0-9a-z][0-9a-z-]{0,23}$, stable for the life of the slide
type ElementId = string;            // ^e-[0-9A-Za-z][0-9A-Za-z-]{0,31}$ (the SDK also emits e-raw-<n>, e-ftr, e-sldNum, e-dt)
```

### 2.2 Slide

```ts
interface Slide {
  id: SlideId;
  kind: "layout" | "generated";
  layout: string;                    // layout name from the template description
  placeholders: Record<PlaceholderKey, PlaceholderContent>;   // keyed by role ("title", "body", "subtitle", "picture", ...) or "idx:<n>"
  elements: FreeElement[];           // only where the contract allows (rule tpl.layout-only)
  notes?: Text;
  locked?: boolean;
  locked_elements?: ElementId[];
  generated?: { script: string };    // kind = "generated": Python source run with present_sdk (section 5)
  hidden?: boolean;
  tags?: string[];
}
type PlaceholderKey = string;

type PlaceholderContent =
  | { type: "text", id: ElementId, text: Text }
  | { type: "image", id: ElementId, asset: AssetRef, alt: string, fit?: "fill"|"fit"|"crop", crop?: Crop, cite?: Cite[] }
  | { type: "chart", id: ElementId, chart: Chart }
  | { type: "table", id: ElementId, table: TableEl }
  | { type: "diagram", id: ElementId, diagram: Diagram }
  | { type: "empty" };

interface FreeElement { id: ElementId; x: number; y: number; cx: number; cy: number;
  content: Exclude<PlaceholderContent, { type: "empty" }> | { type: "shape", id: ElementId, shape: "rect"|"rounded-rect"|"ellipse"|"line"|"arrow", fill?: ThemeColor, line?: ThemeColor, text?: Text } }

interface Text { paragraphs: Paragraph[] }
interface Paragraph { level?: 0|1|2|3|4; runs: Run[]; bullet?: "auto"|"none"|"number"; align?: "auto"|"left"|"center"|"right" }
interface Run { t: string; b?: boolean; i?: boolean; u?: boolean; color?: ThemeColor; link?: string; cite?: Cite[] }
type ThemeColor = "dk1"|"lt1"|"dk2"|"lt2"|"accent1"|"accent2"|"accent3"|"accent4"|"accent5"|"accent6"|"hlink"|"folHlink";
type AssetRef = string;              // "assets/<sha256>.<ext>"
interface Crop { l: number; t: number; r: number; b: number }   // fractions 0..1
```

### 2.3 Citations

```ts
type Cite = string;    // "ref:<ref_id>@<version>#<loc>"
```

`ref_id` is `^r-[0-9a-z][0-9a-z-]{0,23}$`; `version` is the first 12 hex digits of the reference file's SHA-256. Location grammar (`loc`), by reference kind:

| kind | loc | example |
|---|---|---|
| pdf | `p<page>[:c<start>-<end>]` (character offsets within the page's extracted text) | `p11:c120-188` |
| docx, markdown, text, html, eml, msg | `c<start>-<end>` over the extracted plain text; `att<n>/...` addresses an attachment's own loc | `c3400-3520` |
| pptx | `s<slide>[/sh<shape_id>]` | `s4/sh12` |
| xlsx | `<sheet>!<A1 range>` or `name:<named range>` | `Q3!B2:F14` |
| csv | `r<row1>-<row2>[:c<col1>-<col2>]` (1-based) | `r1-13` |
| image | `full` or `box:<x>,<y>,<w>,<h>` (fractions) | `full` |
| analyst-figure, analyst-table | `run:<run_id>/<name>` | `run:20261104-1132-ab12/residuals` |
| any extracted table | `<base loc>/t<n>` | `p12/t1` |

### 2.4 Charts

```ts
interface Chart {
  type: "bar"|"column"|"line"|"pie"|"doughnut"|"scatter"|"area"|"combo";
  stacked?: "none"|"stacked"|"percent";
  source: ChartSource;
  steps?: TransformStep[];           // section 2.6
  series_from: "columns" | "rows";
  category?: string;                 // column name for categories (x)
  series?: { name: string, column: string, type?: "bar"|"line", axis?: "primary"|"secondary", color?: ThemeColor }[];
  title?: string; x_title?: string; y_title?: string;
  number_format?: string;            // Excel format code, e.g. "0.0%"
  labels?: boolean; legend?: "none"|"right"|"bottom"|"top";
  computed?: Table;                  // written by the core after chart.compute; rendered values
  computed_at_version?: string;      // reference version the values were computed from
}
type ChartSource =
  | { kind: "ref", cite: Cite }                       // a range, named range, or extracted table
  | { kind: "inline", table: Table, cites: Cite[] }   // literal data; one cite per row or per value (rule src.coverage)
```

### 2.5 Tables and diagrams

```ts
interface TableEl { columns: number; header_row: boolean; banded_rows: boolean;
  rows: { cells: { text: Text, span?: number }[] }[]; source?: ChartSource; steps?: TransformStep[] }

type Diagram =
  | { family: "process", variant: "chevrons"|"steps"|"flowchart", nodes: DNode[], edges?: { from: string, to: string, label?: string }[] }
  | { family: "timeline", variant: "milestones"|"swimlanes"|"gantt", items: { id: string, label: Text, start: string, end?: string, lane?: string }[], lanes?: string[], axis?: { from: string, to: string, unit: "day"|"week"|"month"|"quarter"|"year" } }
  | { family: "matrix", variant: "2x2"|"grid"|"kpi-tiles", axes?: { x: string, y: string, x_low: string, x_high: string, y_low: string, y_high: string }, rows?: string[], columns?: string[], cells: { row?: number, col?: number, x?: number, y?: number, label: Text, score?: 0|1|2|3|4, value?: string }[] }
  | { family: "hierarchy", variant: "org"|"tree"|"driver-tree", nodes: (DNode & { parent?: string, value?: string })[] };
interface DNode { id: string; label: Text; detail?: Text; kind?: "step"|"decision"|"start"|"end" }
```

Dates are ISO 8601 (`2026-03`, `2026-03-15`, `2026-Q1` accepted). Diagram layout is computed by the kernel (section 5.3) and rendered as a grouped set of shapes named with the element id; the agent never supplies coordinates for diagrams.

### 2.6 Transforms

Closed set, applied in order to a `Table`; implemented in `present_kernel.transforms` with pandas; each step is pure.

```ts
type TransformStep =
  | { op: "select", columns: string[] }
  | { op: "rename", map: Record<string, string> }
  | { op: "filter", column: string, cmp: "=="|"!="|"<"|"<="|">"|">="|"in"|"not_in"|"contains", value: unknown }
  | { op: "sort", by: string[], descending?: boolean }
  | { op: "top_n", n: number, by: string, other_label?: string }        // remainder summed into other_label when set
  | { op: "group_sum"|"group_mean"|"group_count", by: string[], columns?: string[] }
  | { op: "pivot", index: string, columns: string, values: string, agg?: "sum"|"mean"|"count" }
  | { op: "unpivot", id_columns: string[], var_name: string, value_name: string }
  | { op: "pct_change", column: string, periods?: number, as?: string }
  | { op: "share_of_total", column: string, as?: string }
  | { op: "cumulative", column: string, as?: string }
  | { op: "scale", columns: string[], factor: number }                 // e.g. 0.001 for thousands
  | { op: "round", columns: string[], digits: number };
```

### 2.7 Identity in the exported file

Every rendered element's shape gets `name="<element_id>"` and a `p:custDataLst` tag `present.element = <element_id>`; every slide gets the tag `present.slide = <slide_id>`. Document custom properties: `present.deck_snapshot = <snapshot_id>`, `present.schema_version = 1`, `present.mode = structured|code`. Citations are written as tags `present.cite.<n> = <Cite>` on the element's shape (and on chart graphic frames). An export option `strip_tags` removes all `present.*` tags and properties.

## 3. Template description

```ts
interface TemplateDescription {
  path: string; sha256: string;
  slide_size: { cx: number, cy: number };
  theme: { name: string, colors: Record<ThemeColor, string /*hex*/>, fonts: { major: string, minor: string, ea?: string, cs?: string } };
  masters: { name: string, layouts: LayoutInfo[] }[];
  table_styles: { id: string, name: string }[];
  default_table_style?: string;      // the template's default table style id, applied to new tables
  fonts_missing: string[];           // theme or placeholder fonts not installed on this machine
}
interface LayoutInfo {
  name: string; master: string; index: number;
  description?: string;              // from the sidecar, or generated by the agent and cached
  placeholders: PlaceholderInfo[];
  usable: boolean;                   // false if the sidecar marks it avoid, or it has no fillable placeholders
}
interface PlaceholderInfo {
  key: PlaceholderKey;               // role name, made unique with a suffix ("body", "body-2") or "idx:<n>"
  type: "title"|"ctrTitle"|"subTitle"|"body"|"obj"|"pic"|"chart"|"tbl"|"dgm"|"media"|"clipArt"|"dt"|"ftr"|"sldNum";
  idx: number; x: number; y: number; cx: number; cy: number;
  autofit: "none"|"shrink"|"resize";
  anchor: string;                    // vertical anchor of the text: "t" | "ctr" | "b"
  levels: { level: number, font: string, size_pt: number, bold: boolean, color: ThemeColor|string, bullet: boolean, space_before_pt: number, line_spacing: number, mar_l: number }[];
  insets: { l: number, t: number, r: number, b: number };
  accepts: ("text"|"image"|"chart"|"table"|"diagram")[];
}
```

Cached at `.present/cache/template-<sha256>.json`. The agent receives a compact rendering (layout name, description, placeholder keys with `accepts` and approximate capacity in characters per level) via `template.layouts`.

## 4. Project files and global configuration

### 4.1 Global `config.toml`

Location: `~/.present/config.toml` (macOS, Linux) or `%APPDATA%\Present\config.toml` (Windows); override with `--home` / `PRESENT_HOME`.

```toml
[defaults]
model = "gateway/claude-opus-5-5"     # "<provider name>/<model id>"

[network]
proxy = "system"                      # system | none | "http://proxy.corp:8080"
ca_bundle = "system"                  # system | path to a PEM file (added to the OS trust store, not replacing it)

[[providers]]
name = "gateway"
kind = "anthropic"                    # anthropic | openai-compatible
base_url = "https://llm-gateway.corp.example"
auth = "bearer"                       # x-api-key (default for anthropic) | bearer | none
key = "keychain:gateway"              # keychain:<entry> | env:<VAR>
headers = { "X-Team" = "strategy" }   # extra headers, sent on every request
location = "internal"                 # internal | external: shown in the chat header

[[providers]]
name = "gpu"
kind = "openai-compatible"
base_url = "http://gpu01.corp:8000/v1"
auth = "none"
location = "internal"

[[models]]
provider = "gateway"
id = "claude-opus-5-5"                # sent verbatim; may be a gateway alias
context_window = 1000000              # optional override of the probe
vision = true                         # optional override
max_output_tokens = 64000

[embedding]
kind = "bundled"                      # bundled | endpoint
# provider = "gpu"; model = "..."     # when kind = "endpoint" (OpenAI-compatible /embeddings)

[render]
engine = "auto"                       # auto | powerpoint | libreoffice | none
libreoffice_path = ""                 # optional explicit soffice path

[ui]
theme = "system"                      # system | light | dark
```

Keys live in the OS keychain (`keyring` crate: Keychain on macOS, Credential Manager on Windows). `env:` is for development and CI.

### 4.2 Project `.present/project.toml`

```toml
[project]
name = "Q3 review"
created = "2026-11-15T10:00:00Z"

[policy]
edits = "review"                      # review | auto-new-slides | auto
code = "ask"                          # builds and generated slides: ask | auto | deny
skills_scripts = "ask"                # ask | auto | deny
code_mode = "allow"                   # allow | deny (switching to code generation mode)
mcp = "ask"
export = "auto"
max_steps = 60
build_timeout_s = 60

[contract]                            # rule id = severity (section 7); omitted rules use defaults
"density.bullets" = "warn"
"fit.overflow" = "error"
max_bullets = 6
max_words = 70
min_font_pt = 12
allow_free_elements = false

[[export_presets]]
name = "pre-read"
format = "pdf"
dpi = 150
file = "{project}-{date}-preread.pdf"
```

### 4.3 Reference manifest `references/manifest.toml`

```toml
[[refs]]
id = "r-q3rep"
file = "references/3f9a1c2b7e01.pdf"   # <first 12 hex of sha256>.<ext>; versions are separate files
kind = "pdf"
title = "Q3 report"
original_path = "C:/Users/.../Q3 report.pdf"
imported = "2026-11-15T10:02:11Z"
version = "3f9a1c2b7e01"
previous_versions = []
parent = ""                            # set for email attachments
source = "file"                        # file | analyst
analyst = { repo = "", item = "", run_id = "" }
```

### 4.4 Snapshot store `.present/history/`

```
objects/<aa>/<sha256>           zstd-compressed blobs (slide JSON, deck.json, build.py and modules)
snapshots/<snapshot_id>.json    manifest
log.jsonl                       append-only: { ts, snapshot_id, parent, reason: "turn"|"mode-switch"|"export"|"restore"|"manual", turn_id?, label? }
```

Manifest: `{ snapshot_id, created, parent, mode, files: { path: sha256 }[], assets: sha256[], refs: { id, version }[] }`. `snapshot_id` is `^h-[0-9a-z]{1,16}$` (the core generates 10 characters). Assets and references are already content-addressed in the project and are only listed, never copied. Restore writes the files of a snapshot to `deck/` and records a new `restore` snapshot (history is append-only). Pruning keeps every `mode-switch` and `export` snapshot, the last 200 `turn` snapshots, and all from the last 14 days.

### 4.5 Project tree

As ARCHITECTURE.md section 15, plus `.present/cache/` (template descriptions, thumbnails, fidelity renders keyed by output sha256; safe to delete).

## 5. Deck SDK (`present_sdk`)

Pure Python, depends only on `python-pptx`, `lxml`, `Pillow`. Used by the renderer (structured mode), by eject output, by code-mode build scripts, and by generated slides. Stable public API, versioned with `schema_version`.

### 5.1 Public API

```python
from present_sdk import Deck, Slide, theme, cite, alt, Emu, Pt

deck = Deck.open_template()                  # template path and project root come from the environment set by the kernel
deck.properties["Title"] = "Q3 review"

s = deck.add_slide(layout="Title and Content", slide_id="s-7f3a")   # slide_id optional in code mode (stable id derived from call order + layout otherwise)
s.title("Margin recovered to 18% on pricing actions")
body = s.placeholder("body")
p = body.paragraph("Gross margin up 3.1 pts quarter on quarter", level=0)
cite(p.runs[0], "ref:r-q3rep@3f9a1c2b7e01#p11:c120-188")
s.notes("Pause on the EMEA point.")

s2 = deck.add_slide(layout="Title Only")
s2.title("Revenue by region")
ch = s2.chart(placeholder="body", type="column",
              data=deck.ref_table("ref:r-pipe@aa01b2c3d4e5#Pipeline!A1:E9"),
              steps=[{"op": "group_sum", "by": ["Region"], "columns": ["Revenue"]}],
              category="Region", series=[{"name": "Revenue", "column": "Revenue"}])
s2.diagram("process", variant="chevrons", nodes=[...], box=s2.free_box())   # same primitives as the schema

theme.color("accent1"); theme.font("major")   # never hex or font names in scripts (lint rule code.hardcoded-style)
deck.save()                                    # writes to the output path provided by the kernel
```

### 5.2 Rules for scripts

- Scripts receive `PRESENT_PROJECT`, `PRESENT_TEMPLATE`, `PRESENT_OUT`, `PRESENT_SNAPSHOT` in the environment; `Deck.open_template()` and `deck.save()` use them.
- Raw `python-pptx` and `lxml` are allowed (that is the point of code mode); the SDK exposes `s.raw` (the `python-pptx` slide) for it.
- Reading references: `deck.ref_table(cite)`, `deck.ref_text(cite)`, `deck.asset(path)`; direct file access is limited by the sandbox to `template/`, `assets/`, `deck/`, `references/`.
- Every slide built by a script gets a stable id (explicit or derived) and every shape created through the SDK gets an element id; shapes created raw get ids assigned at save (`e-raw-<n>`), so the element map is always complete.

### 5.3 Diagram layout

`present_sdk.diagrams` computes positions inside a box (placeholder or free box) from content only: process (horizontal or wrapped rows; flowchart uses a layered layout with orthogonal connectors), timeline (axis scaling from `axis` or the item range; lane assignment), matrix (equal grid; 2x2 plots items by `x`,`y` in 0..1), hierarchy (tidy tree, top-down; org charts wrap wide levels). Text inside shapes uses theme fonts and auto-sizes by the same measurement as `text.measure`, shrinking down to `min_font_pt` then reporting `fit.overflow`.

### 5.4 Eject

`deck.eject` emits `deck/build.py` with a header comment, one function `slide_<n>_<slug>(deck)` per slide reproducing the structured content through the SDK (generated slides' scripts inlined as functions), and a `main()` calling them in order. Verification: build the script into a temporary file and compare against `deck.render` of the same snapshot, slide by slide, on the XML of each slide part after normalizing ids and relationship numbering; `verified: false` with a diff when they differ. The switch is refused when not verified.

## 6. Agent, model, and transcript

### 6.1 Model wire mapping

Identical to Analyst PROTOCOL section 4 (tool name mapping `.` to `_` and `mcp.<server>.<tool>` to `mcp__<server>__<tool>`, both adapters, the internal `ModelEvent` stream, the text-protocol fallback), with these additions:

- **Auth and headers:** `auth = "bearer"` sends `Authorization: Bearer <key>` instead of `x-api-key`; `headers` are added to every request.
- **Network:** requests go through the configured proxy and trust store (`reqwest` with `rustls-platform-verifier`; system proxy discovery including PAC through the OS on Windows and macOS).
- **Unknown-field downgrade:** if an endpoint answers 400 naming a request field the adapter sent optionally (`stream_options`, `cache_control`, `citations`, document blocks), the adapter retries once without it and records the field as unsupported in the capability record.
- **Document blocks:** the Anthropic adapter can send `{type: "document", source: {type: "base64", media_type: "application/pdf", data}, citations: {enabled: true}, title}` blocks and maps returned `citations` (`page_location`) to `Cite` strings; used only when the capability record says `documents: true`.
- **Prompt caching:** the Anthropic adapter puts `cache_control: {type: "ephemeral"}` on the last system block and the last tool definition when `caching: true` in the capability record.
- **Capability probe** adds a vision probe (a 64×64 PNG with a two-digit number the model must read back) and records `vision`, `documents`, `caching`. Baseline (ARCHITECTURE section 11): `tools = native`, `context_window >= 131072`, `vision = true`; anything else is flagged `degraded`.

### 6.2 Transcript events

Analyst PROTOCOL section 5 events, with `checkpoint.ref` replaced by `snapshot_id`, and these additions:

```ts
  | { ts, type: "proposal", id: string, kind: "slide-create"|"slide-update"|"slide-delete"|"slide-move"|"outline"|"code-edit"|"build"|"generated-slide"|"skill-script"|"mode-switch"|"export", summary: string, slides?: SlideId[], diff?: string }
  | { ts, type: "snapshot", snapshot_id: string, reason: string }
  | { ts, type: "render", out: string, sha256: string, engine?: string }
  | { ts, type: "check", diagnostics: number, errors: number, slides: SlideId[] }
  | { ts, type: "skill_loaded", skill: string }
  | { ts, type: "export", format: string, out: string, snapshot_id: string }
  | { ts, type: "summary", text: string, slides_changed: SlideId[], exports: string[] }
  | { ts, type: "compaction", text: string, kept_messages: number }       // lets a compacted conversation replay to the same history
  // `decision` gains `modified?: boolean` (accepted with the user's edits); `proposal` is written with `targets` (read as `slides` too)
```

### 6.3 Agent tools (phase 0 set)

Every result is capped at 16 KiB in context, with the same truncation and artifact paging rule as Analyst.

| tool | args | policy class | backed by |
|---|---|---|---|
| `deck.outline` | `{}` | read | core: one line per slide (id, layout, title, lock, check status) |
| `deck.read_slide` | `{slide_id}` | read | core |
| `deck.search` | `{query}` | read | core: text search across slides and notes |
| `template.layouts` | `{}` | read | kernel `template.inspect`, compact form |
| `template.layout_detail` | `{layout}` | read | kernel |
| `refs.list` | `{}` | read | core: refs with outlines (depth 2) |
| `refs.search` | `{query, ref_ids?, k?}` | read | core: hybrid search (section 8) |
| `refs.read` | `{cite, max_chars?}` | read | kernel |
| `refs.table` | `{cite}` | read | kernel |
| `refs.image` | `{cite}` | read | kernel; returns an image block |
| `slide.render` | `{slide_id}` | read | kernel fidelity render (or HTML raster); returns an image block |
| `check.run` | `{slide_ids?}` | read | kernel |
| `chart.preview` | `{source, steps}` | read | kernel `chart.compute` |
| `outline.propose` | `{rows: {slide_id?, title, message, layout?, sources?: Cite[]}[]}` | edit | core (Outline mode) |
| `slide.propose_create` | `{after?: SlideId, section?: string, slide: Slide}` | edit | core |
| `slide.propose_update` | `{slide_id, slide?: Slide, patch?: JsonPatch}` | edit | core; `patch` is RFC 6902 |
| `slide.propose_delete` | `{slide_id}` | edit | core |
| `slide.propose_move` | `{slide_id, after?: SlideId, section?: string}` | edit | core |
| `asset.import` | `{cite?: Cite, analyst_item?: string}` | edit | core + kernel |
| `code.read` | `{path?}` | read | core (code mode, generated slides) |
| `code.propose_edit` | `{path, edits: {old, new}[]}` or `{path, content}` | edit | core (Analyst `fs.propose_edit` semantics) |
| `deck.build` | `{}` | code | kernel (code mode) |
| `deck.switch_mode` | `{to: "code"}` | edit, always ask | core + kernel `deck.eject` |
| `skill.load` | `{name}` | read | core |
| `skill.read_resource` | `{name, path}` | read | core |
| `skill.run_script` | `{name, script, args?}` | skills_scripts | kernel sandbox |
| `export.run` | `{preset?: string, format?: string}` | export | core + kernel |

Model-facing names follow 6.1 (`deck_outline`, `slide_propose_update`, ...).

### 6.4 Template sidecar `template/template.toml`

```toml
[template]
name = "Corp 2026"
owner = "Brand team"

[layouts."Title and Content"]
use_for = "One message with up to five supporting points"
max_bullets = 5

[layouts."Layout 7"]
description = "Big number with caption"
use_for = "A single headline metric"

[layouts."Blank"]
avoid = true

[rules]
mandatory = ["Title Slide", "Disclaimer"]   # layouts that must appear at least once
closing = "Closing"                          # layout of the last slide
allow_free_elements = false
icons_dir = "icons"                          # relative to template/
```

## 7. Deck checker rules

Ids, default severities, and inputs. Severities are overridable in `[contract]`.

| id | default | applies to | check |
|---|---|---|---|
| `tpl.layout-only` | error | structured | layout exists in the template; free elements only if allowed |
| `tpl.theme-fonts` | error | both | every run's resolved font is a theme font or in an allow list |
| `tpl.theme-colors` | warn | both | every explicit color is a theme color |
| `tpl.mandatory` | warn | both | sidecar mandatory and closing layouts present |
| `fit.overflow` | error | both | text fits its box per `text.measure` with the box's autofit |
| `fit.min-size` | warn | both | effective size after shrink >= `min_font_pt` |
| `density.bullets` | warn | both | bullets per slide <= `max_bullets` (sidecar per layout overrides) |
| `density.words` | warn | both | words per slide (excluding notes) <= `max_words` |
| `src.coverage` | warn | both | each paragraph flagged factual, each chart, and each table has >= 1 citation |
| `src.numbers` | warn | both | numeric tokens in cited runs appear in the cited passage (section 8.3) |
| `src.chart-stale` | warn | both | chart `computed` equals `chart.compute` at the current reference version |
| `src.dangling` | error | both | every citation resolves to an existing reference version and location |
| `a11y.alt-text` | error | both | pictures and charts have non-empty alt text |
| `a11y.contrast` | warn | both | text contrast against its resolved fill >= 4.5:1 (3:1 for >= 18 pt) |
| `a11y.reading-order` | info | both | z-order matches a top-left to bottom-right reading order or is explicit |
| `struct.titles` | error | both | every visible slide has a unique non-empty title |
| `img.resolution` | warn | both | effective image DPI at placed size >= 150 |
| `code.hardcoded-style` | warn | code | literal hex colors or font names in scripts (AST check) |
| `out.opens-clean` | error | both | the `.pptx` re-opens in `python-pptx`, validates against the ECMA-376 transitional schemas when they are installed (`vendor/ooxml-schemas`), and converts in the fidelity engine, with one page per visible slide, when an engine is present |

"Factual" paragraphs: a paragraph is factual if it contains a digit, a currency or percent sign, or a proper noun outside the title, or if the agent tagged it `cite` at all; the agent can mark a paragraph non-factual with an empty `cite: []`, which the review card shows.

## 8. References, search, and the number check

### 8.1 Index (core-owned SQLite, `index.sqlite`)

```sql
CREATE TABLE refs (id TEXT PRIMARY KEY, version TEXT, kind TEXT, title TEXT, file TEXT, outline_json TEXT, status TEXT);
CREATE TABLE chunks (rowid INTEGER PRIMARY KEY, ref_id TEXT, version TEXT, loc TEXT, heading_path TEXT, text TEXT);
CREATE VIRTUAL TABLE chunks_fts USING fts5(text, heading_path, content='chunks', content_rowid='rowid', tokenize='unicode61 remove_diacritics 2');
CREATE TABLE chunks_vec (rowid INTEGER PRIMARY KEY, dims INTEGER NOT NULL, embedding BLOB NOT NULL);   -- unit-length little-endian f32; searched by exact cosine similarity in the core
```

Chunking: 800 to 1200 characters on paragraph boundaries within a heading section, 150-character overlap, never across pages for PDFs. Only the current version of each reference is searchable; older versions stay resolvable for citations.

### 8.2 Hybrid search

`refs.search`: BM25 top 50 and vector top 50, reciprocal rank fusion (k = 60), at most 3 hits per reference unless `ref_ids` narrows the set, return top `k` (default 8) as `{cite, title, heading_path, snippet}`.

### 8.3 Number check

Tokenize numeric expressions in the run (`-3.1`, `18%`, `€1.2bn`, `1,234`, `0.18`, `Q3`-style ordinals excluded). Normalize to a value and a scale (percent ↔ fraction, k/m/bn/thousand/million/billion, currency symbols stripped and kept as a unit). Search the cited passage (the loc's text, or the cited table's cells) for an equal value: equal after normalization is **green**; equal after rounding to the run's displayed precision is **amber**; absent is **red**; a run with numbers and no citation is **grey**. Derived values (a difference of two cited numbers) are amber only if the agent supplies both cites.

## 9. Code sandbox

The kernel runs builds, generated slides, and skill scripts as a child process: `<runtime_python> -I -m present_kernel.runner <job.json>`, wrapped as below. Environment: a minimal allowlist plus the `PRESENT_*` variables; `HOME`/`USERPROFILE` point at the scratch directory; no credentials.

| Platform | Mechanism | Read | Write | Network |
|---|---|---|---|---|
| macOS | `sandbox-exec -p <profile>` (Analyst's profile generator, from `wb-sandbox`) | runtime, `template/`, `assets/`, `deck/`, `references/`, skill folder | scratch dir, output path | denied |
| Windows | AppContainer (`CreateAppContainerProfile`, no capabilities) + job object (memory, CPU time, active process limit, UI restrictions, kill on close); ACLs granted on a per-run copy-free view: read ACEs for the AppContainer SID on the listed folders, write on scratch | same | same | denied (no `internetClient` capabilities) |
| Linux (phase 4) | `bwrap` (Analyst's) | same | same | `--unshare-net` |
| Fallback | resource limits only, `sandbox: "limits"` | — | — | not enforced; badged; refusable by config |

Limits: `build_timeout_s` (default 60), 2 GiB memory, 64 processes. The output `.pptx` is written to the scratch dir and moved into place by the kernel only on exit code 0.

## 10. Audit log

`<home>/audit/YYYY-MM.jsonl`, one record per model request:

```ts
{ ts, project: string, conversation: string, turn: string, provider: string, location: "internal"|"external",
  base_url_host: string, model: string, input_tokens: number, output_tokens: number,
  refs: { cite: Cite, chars: number }[],    // excerpts included (tool results and document blocks)
  images: { source: "slide"|"ref"|"asset", id: string }[],
  status: "ok"|"error"|"refused", error?: string }
```

Never contains message content, keys, or headers. Rotated monthly; the UI exports a date range as CSV.

## 11. Skills

Agent Skills folders: `SKILL.md` with YAML front matter `name` (`^[a-z0-9-]{1,64}$`), `description` (<= 1024 chars), optional `license`, `allowed-tools` (ignored), plus Analyst legacy fields `triggers`, `paths`, `always` (honored by Analyst, ignored by Present). Resources are any files in the folder; scripts are files under `scripts/` with extension `.py` (other languages are not run). Discovery order (later wins on name clash, managed library cannot be overridden): built-in, `<home>/skills/`, `<project>/.present/skills/`, managed library (phase 3). Context: names and descriptions only in the system prompt; `skill.load` returns the body; `skill.read_resource` reads a file in the folder (text up to the cap, images as image blocks).

## 12. Evaluation tasks

Same layout and harness as Analyst PROTOCOL section 8: `evals/tasks/<name>/task.toml` plus `fixture/` (a project folder with `template/`, `references/`, `.present/project.toml`, and an optional starting `deck/`), run with `present eval run`. Check kinds:

| kind | params | passes when |
|---|---|---|
| `opens_clean` | — | the exported `.pptx` passes `out.opens-clean` |
| `checker_clean` | `max_warnings?` | no errors; warnings <= max |
| `slide_count` | `min?`, `max?`, `exact?` | visible slides within range |
| `layout_used` | `layout`, `min?` | layout used at least `min` times |
| `uses_native` | `element: "chart"|"table"|"diagram"`, `min?` | that many native elements exist in the export |
| `citation_coverage` | `min` (0..1) | factual paragraphs with citations / factual paragraphs >= min |
| `numbers_verified` | `min` (0..1) | green or amber numbers / cited numbers >= min |
| `text_matches` | `slide?`, `regex` | a slide's text (or any) matches |
| `locked_untouched` | — | locked slides are byte-identical to the fixture |
| `mode` | `mode` | final deck mode |
| `export_exists` | `format` | an export of that format exists and is non-empty |
| `edits_within_scope` | — | only declared slides changed |

`evals/fake_model_server.py` is reused from Analyst (OpenAI-compatible, stdlib only) with scripted plays for Present tasks.

**Fixture layout.** `fixture/TEMPLATE` holds the path of a template under `evals/templates` (for example `corp/corp.potx`); `fixture/sources/` holds files the harness adds as references; `fixture/deck/` is an optional starting deck (its `template` field is rewritten); `fixture/assets/` and `fixture/project.toml` are copied when present. The harness builds a fresh project from these for every run.
