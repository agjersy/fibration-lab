# Desktop IPC Contract

> **Historical design note.** This document describes Personal Present before it was folded into Fibration Lab. Where it conflicts with the documents in [`docs/`](../../docs/) (one core, one `fibration` CLI and desktop app, decks under `decks/<slug>/` in the project, git checkpoints instead of snapshots, the artifact index instead of the scan/import bridge, `~/.fibration` instead of `~/.present`), `docs/` wins. `PRESENT_HOME` and the `present` binary were removed.

*Status: phase 1 baseline. Normative for `apps/desktop/src-tauri` (Rust) and `apps/desktop/src` (React). Shared types live in `crates/protocol/src/ui.rs` with `ts-rs` export to `apps/desktop/src/ipc/generated/`. Deck, template, diagnostic, and citation types are the ones in [PROTOCOL.md](PROTOCOL.md) and are re-exported unchanged. The boundary's shape is the same as Fibration Lab's IPC.md section 1; only the content differs.*

## 1. Shape of the boundary

Same as Analyst:

- Commands (Tauri `invoke`) with named arguments returning JSON or `{ code, message, detail? }`. Error codes: `not_found`, `conflict`, `policy_denied`, `kernel_offline`, `engine_unavailable`, `sandbox_unavailable`, `invalid`, `io`, `internal`.
- Per-project event channel (`project_subscribe`), ordered per project, one per open project per window.
- Long operations return a `task_id` and report through `task_progress` and `task_done`.
- The renderer never touches files, processes, or the network. In a plain browser it runs against `MockBackend` (section 6).
- The `Backend` interface (`invoke`, `subscribe`) comes from `@wb/ui` (shared with Analyst).

## 2. Types

```ts
type ProjectId = string; type ConversationId = string; type TurnId = string;
type ProposalId = string; type TaskId = string; type SnapshotId = string;
type Mode = "outline" | "build" | "polish";
type DeckMode = "structured" | "code";
type KernelState = "stopped" | "starting" | "ready" | "error";

interface AppInfo { version: string; home_abs: string; platform: "macos" | "windows" | "linux"; dev_mode: boolean;
  engines: EngineInfo[]; sandbox: SandboxInfo; embedding: EmbeddingInfo;
  kernel_network: "bwrap" | "sandbox-exec" | "none"; kernel_network_detail: string | null }  // how the deck kernel is kept off the network

interface ProjectSummary { id: ProjectId; name: string; path_abs: string; template_name: string; deck_mode: DeckMode;
  slides: number; model: string | null; created_at: string; last_opened_at: string; open: boolean }

interface PolicyView { edits: "review" | "auto-new-slides" | "auto" | "deny"; code: "ask" | "auto" | "deny";
  skills_scripts: "ask" | "auto" | "deny"; code_mode: "allow" | "deny";
  export: "ask" | "auto"; max_steps: number }

interface ConversationSummary { id: ConversationId; title: string; model: string | null; mode: Mode; created_at: string; turns: number; busy: boolean }

interface ProjectInfo extends ProjectSummary {
  policy: PolicyView; kernel: KernelState; conversations: ConversationSummary[]; active_conversation: ConversationId;
  template: TemplateDescription; deck: Deck; head_snapshot: SnapshotId }

interface SlideView {
  slide: Slide;                         // structured mode; in code mode a synthesized read-only Slide from the element map
  index: number; section: string;
  thumb: { kind: "fidelity" | "preview" | "none"; path: string | null; stale: boolean };   // project-relative PNG under .present/cache/
  checks: { errors: number; warnings: number };
  numbers: { green: number; amber: number; red: number; grey: number };
}

interface RefView { id: string; version: string; kind: RefKind; title: string; size_bytes: number; pages: number | null;
  status: "queued" | "extracting" | "embedding" | "ready" | "error"; error: string | null; cited_by: SlideId[]; parent: string | null }

interface SourcePassage { cite: Cite; ref_title: string; kind: RefKind;
  text: string | null; highlight: [number, number] | null;  // character range within text
  page_png: string | null;                                    // PDF page or pptx slide image, for visual sources
  table: Table | null; highlight_cells: [number, number, number, number] | null }

interface ProposalView {
  id: ProposalId; conversation: ConversationId; turn: TurnId;
  kind: "slide-create" | "slide-update" | "slide-delete" | "slide-move" | "outline" | "code-edit" | "build" | "generated-slide" | "skill-script" | "mode-switch" | "export";
  summary: string; reason: "policy" | "out-of-scope" | "locked" | "always-ask" | null;
  slides: { slide_id: SlideId; before: Slide | null; after: Slide | null; before_png: string | null; after_png: string | null; text_diff: string }[];
  outline: OutlineRow[] | null;
  code: { path: string; original: string | null; proposed: string } | null;
  script: { skill: string | null; path: string; args: string[] } | null;
}
interface OutlineRow { slide_id: SlideId | null; title: string; message: string; layout: string | null; sources: Cite[] }

type ProposalDecision =
  | { decision: "accept" }
  | { decision: "accept_modified"; slides?: Slide[]; outline?: OutlineRow[]; code?: string }
  | { decision: "reject"; note?: string };

interface ExportView { id: string; format: string; preset: string | null; path: string; snapshot_id: SnapshotId; created_at: string;
  errors: number; warnings: number; engine: string | null }

interface SnapshotView { id: SnapshotId; created: string; reason: string; label: string | null; turn: TurnId | null; slides: number; thumb: string | null }

interface ModelEntry { name: string /* provider/id */; provider: string; location: "internal" | "external";
  capabilities: { context_window: number; tools: "native" | "none"; vision: boolean; documents: boolean; caching: boolean; probed_at: string | null };
  degraded: boolean; degraded_reasons: string[] }

interface SkillView { name: string; description: string; source: "builtin" | "user" | "project" | "managed"; has_scripts: boolean; enabled: boolean }

interface TranscriptEvent { /* PROTOCOL.md 6.2 */ }
interface AnalystItem { id: string; kind: "figure" | "table"; title: string; run_id: string; script: string; args: string[]; created: string; thumb: string | null }
```

## 3. Events

```ts
type ProjectEvent =
  | { type: "agent_text"; conversation: ConversationId; turn: TurnId; delta: string }
  | { type: "agent_tool_call"; conversation: ConversationId; turn: TurnId; id: string; name: string; arguments: unknown }
  | { type: "agent_tool_result"; conversation: ConversationId; turn: TurnId; id: string; ok: boolean; summary: string; image?: string }
  | { type: "proposal"; proposal: ProposalView }
  | { type: "proposal_resolved"; proposal_id: ProposalId; decision: "accept" | "accept_modified" | "reject" | "auto"; by: "user" | "policy" }
  | { type: "turn_started"; conversation: ConversationId; turn: TurnId }
  | { type: "turn_done"; conversation: ConversationId; turn: TurnId; summary: { text: string; slides_changed: SlideId[]; exports: string[] } }
  | { type: "turn_error"; conversation: ConversationId; turn: TurnId; message: string }
  | { type: "usage"; conversation: ConversationId; input_tokens: number; output_tokens: number; context_window: number; context_used: number }
  | { type: "deck_changed"; deck: Deck; changed: SlideId[]; removed: SlideId[]; head_snapshot: SnapshotId; by: "user" | "agent" | "restore" }
  | { type: "slide_view"; view: SlideView }                    // thumbnail, checks, or number badges updated
  | { type: "diagnostics"; slides: SlideId[]; diagnostics: Diagnostic[] }   // replaces diagnostics for the listed slides
  | { type: "refs_changed"; refs: RefView[] }
  | { type: "build_output"; stream: "stdout" | "stderr"; chunk: string }   // code mode builds, generated slides, skill scripts
  | { type: "export_done"; export: ExportView }
  | { type: "kernel_state"; state: KernelState; message: string | null }
  | { type: "task_progress"; task_id: TaskId; fraction: number | null; message: string }
  | { type: "task_done"; task_id: TaskId; ok: boolean; message: string | null }
  | { type: "project_changed"; info: ProjectInfo };           // model, mode, deck mode, policy, template changes
```

## 4. Commands

| command | args | result | notes |
|---|---|---|---|
| `app_info` | `{}` | `AppInfo` | |
| `projects_list` | `{}` | `ProjectSummary[]` | |
| `project_create` | `{ path_abs, name, template_abs, deck_mode? }` | `ProjectInfo` | copies the template in, inspects it, writes the skeleton |
| `project_open` | `{ path_abs }` | `ProjectInfo` | starts the kernel |
| `project_close` | `{ project }` | `null` | |
| `project_subscribe` | `{ project, channel }` | `null` | |
| `project_set_policy` | `{ project, patch: Partial<PolicyView> }` | `PolicyView` | session override; `persist: true` writes project.toml |
| `project_layout_get` / `_set` | `{ project }` / `{ project, layout }` | `unknown` / `null` | dockview JSON |
| `template_thumbnails` | `{ project }` | `{ layout: string, png: string }[]` | |
| `template_replace` | `{ project, template_abs }` | `TaskId` | re-inspects; diagnostics for slides whose layout is missing |
| `deck_get` | `{ project }` | `{ deck: Deck, slides: SlideView[] }` | |
| `slide_update` | `{ project, slide: Slide, expected_hash }` | `SlideView` | user edit; `conflict` on hash mismatch; locked elements of the agent do not apply to the user |
| `slide_create` | `{ project, after?, layout }` | `SlideView` | |
| `slide_delete` / `slide_duplicate` | `{ project, slide_id }` | `null` / `SlideView` | |
| `slide_move` | `{ project, slide_id, after?, section? }` | `null` | |
| `slide_lock` | `{ project, slide_id, element_id?, locked }` | `null` | |
| `slide_preview_png` | `{ project, slide_id, png_base64 }` | `null` | renderer hands the core a rasterized HTML preview (section 5) |
| `chart_compute` | `{ project, source, steps }` | `Table` | for the chart data sheet |
| `code_read` / `code_write` | `{ project, path }` / `{ project, path, content, expected_hash }` | `FileContent` / `WriteResult` | code mode and generated slides only |
| `deck_build` | `{ project }` | `TaskId` | code mode; streams `build_output` |
| `deck_switch_mode` | `{ project, to: DeckMode, via?: "revert" | "import" }` | `TaskId` | `to: "code"` ejects and verifies; back to structured requires `via` |
| `render_fidelity` | `{ project, slides? }` | `TaskId` | refreshes thumbnails |
| `check_run` | `{ project, slides? }` | `Diagnostic[]` | also emits `diagnostics` |
| `refs_list` | `{ project }` | `RefView[]` | |
| `refs_add` | `{ project, paths_abs: string[] }` | `TaskId` | copies, ingests, embeds; `refs_changed` as each lands |
| `refs_remove` | `{ project, ref_id }` | `null` | `invalid` if cited; `refs_remove_unused` removes all uncited |
| `refs_remove_unused` | `{ project }` | `string[]` | |
| `refs_search` | `{ project, query, ref_ids? }` | `{ cite, title, heading_path, snippet }[]` | |
| `source_passage` | `{ project, cite }` | `SourcePassage` | the source lens |
| `analyst_scan` | `{ project, repo_abs }` | `AnalystItem[]` | |
| `analyst_import` | `{ project, repo_abs, item }` | `RefView` | |
| `assets_list` / `asset_add` | `{ project }` / `{ project, path_abs }` | `{ path, sha256, width, height }[]` / same item | |
| `history_list` | `{ project, limit? }` | `SnapshotView[]` | |
| `history_restore` | `{ project, snapshot_id }` | `SnapshotView` | creates a `restore` snapshot |
| `history_revert_turn` | `{ project, turn }` | `SnapshotView` | |
| `exports_list` | `{ project }` | `ExportView[]` | |
| `export_run` | `{ project, format, preset?, options? }` | `TaskId` | `export_done` on completion |
| `export_reveal` | `{ project, export_id }` | `null` | opens the OS file manager |
| `export_open` | `{ project, export_id }` | `null` | opens with the default app (PowerPoint) |
| `conversation_new` / `_select` / `_transcript` | as Analyst | | |
| `chat_send` | `{ project, conversation, text, attachments?: { slides?: SlideId[], cites?: Cite[], images_abs?: string[] } }` | `TurnId` | |
| `chat_cancel` | `{ project, conversation }` | `null` | pending proposals rejected |
| `chat_set_mode` | `{ project, conversation, mode: Mode }` | `null` | |
| `chat_set_model` | `{ project, conversation, model }` | `null` | |
| `proposal_respond` | `{ project, proposal_id, decision: ProposalDecision }` | `null` | |
| `skills_list` | `{ project }` | `SkillView[]` | |
| `skill_set_enabled` | `{ project, name, enabled }` | `null` | |
| `models_list` | `{}` | `ModelEntry[]` | |
| `model_probe` | `{ model }` | `ModelEntry` | |
| `provider_set_key` | `{ provider, key }` | `null` | writes `<home>/.env` (mode 0600) under the variable of the provider's `key = "env:<VAR>"`; key never returned |
| `audit_export` | `{ from, to, path_abs }` | `null` | CSV |
| `dialog_pick_file` / `dialog_pick_files` / `dialog_pick_directory` / `dialog_save_file` | `{ title?, filters? }` | `string \| string[] \| null` | native dialogs |

## 5. Behavior that both sides rely on

- **User edits are the user's.** `slide_update` writes immediately, takes no proposal, and creates no snapshot of its own; the agent sees user changes through a deck diff against the last turn's snapshot at the start of its next turn.
- **Proposals.** Review mode pauses the turn on `proposal` and resumes on `proposal_respond`. Slide cards show before/after images and a text diff; "accept with my edits" opens the slide editor on the proposed slide and returns `accept_modified`. Code proposals use the CodeMirror merge view with per-hunk accept, as in Analyst. Outline proposals show an editable table.
- **Live preview.** The renderer draws slides from `Slide` + `TemplateDescription` (absolute layout in EMU scaled to the canvas, theme fonts, chart and diagram drawing in SVG). When no fidelity engine exists, the renderer rasterizes the preview of a slide (`html-to-image` on an offscreen node) and sends it with `slide_preview_png` when the core asks for a slide image via `task_progress` message `"need_preview:<slide_id>"`.
- **Thumbnails.** After every accepted change the core schedules a debounced (1.5 s) render of the changed slides with the fidelity engine and emits `slide_view` with `thumb.kind = "fidelity"`; until then thumbnails are `preview` and `stale: true`.
- **Checks.** The core runs `check.run` on changed slides after each accepted change and after `slide_update`, and emits `diagnostics` and `slide_view`.
- **Kernel lifecycle.** Starts on `project_open`; `kernel_state` reports it. Without the kernel, reading, editing, and chat planning still work; rendering, checks, ingest, builds, and exports fail with `kernel_offline`.
- **Layout.** Saved on change (debounced), restored on open.

## 6. MockBackend

`src/ipc/mock.ts`, selected when Tauri is absent. Requirements:

- One in-memory project "Demo review" with a template description of six layouts (Title Slide, Title and Content, Two Content, Title Only, Section Header, Big Number), a theme, and eight structured slides covering text, a column chart with a two-step transform, a table, a `process` and a `timeline` diagram, an image, and notes; one slide locked.
- Three references (a PDF with 12 pages of lorem text and two tables, an XLSX with two sheets, an email with one attachment) with outlines and citations from slides, including one red and one grey number.
- `source_passage` returns text with a highlight for PDF cites and a table with highlighted cells for XLSX cites.
- `chat_send`: in outline mode streams text and emits an `outline` proposal of six rows; in build mode emits a `slide-update` proposal on slide 3 (adds a bullet with a citation) and, after acceptance, a `slide_view` with updated badges and `turn_done`; text containing `code` switches to a `code-edit` proposal on `deck/build.py`; text containing `error` produces `turn_error`.
- `export_run` emits `task_progress` three times and `export_done`; `render_fidelity` flips thumbnails from `preview` to `fidelity` (pre-rendered PNGs in `src/ipc/mock/assets/`).
- `models_list` returns one internal gateway model meeting the baseline and one degraded open-weight model (no vision).
- Latency simulated at 50 to 300 ms.
