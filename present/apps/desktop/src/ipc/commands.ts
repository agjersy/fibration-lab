// Command table from docs/IPC.md section 4. Argument names are exact (snake_case). The Rust side is
// apps/desktop/src-tauri/src/commands.rs, thin glue over `present_core::service`; it registers exactly
// these names.
import type {
  AnalystItemView,
  AppInfo,
  AssetView,
  ChartSource,
  ChatAttachments,
  ConversationSummary,
  DeckMode,
  DeckView,
  Diagnostic,
  DialogFilter,
  ExportView,
  FileContent,
  LayoutThumbnail,
  Mode,
  ModelEntry,
  PolicyPatch,
  PolicyView,
  ProjectInfo,
  ProjectSummary,
  ProposalDecision,
  RefView,
  SearchHit,
  SkillView,
  Slide,
  SlideView,
  SnapshotView,
  SourcePassage,
  Table,
  TranscriptEvent,
  TransformStep,
  WriteResult,
} from "./types";

type ProjectId = string;
type SlideId = string;
type Cite = string;
type TaskId = string;
type TurnId = string;
type ConversationId = string;
type ProposalId = string;
type SnapshotId = string;
type P = { project: ProjectId };
type DialogArgs = { title?: string; filters?: DialogFilter[] };

export interface Commands {
  app_info: { args: Record<string, never>; result: AppInfo };
  projects_list: { args: Record<string, never>; result: ProjectSummary[] };
  project_create: { args: { path_abs: string; name: string; template_abs: string; deck_mode?: DeckMode }; result: ProjectInfo };
  project_open: { args: { path_abs: string }; result: ProjectInfo };
  project_close: { args: P; result: null };
  project_subscribe: { args: P & { channel: unknown }; result: null };
  project_set_policy: { args: P & { patch: PolicyPatch; persist?: boolean }; result: PolicyView };
  project_layout_get: { args: P; result: unknown };
  project_layout_set: { args: P & { layout: unknown }; result: null };
  template_thumbnails: { args: P; result: LayoutThumbnail[] };
  template_replace: { args: P & { template_abs: string }; result: TaskId };
  deck_get: { args: P; result: DeckView };
  slide_update: { args: P & { slide: Slide; expected_hash?: string }; result: SlideView };
  slide_create: { args: P & { after?: SlideId; layout: string }; result: SlideView };
  slide_delete: { args: P & { slide_id: SlideId }; result: null };
  slide_duplicate: { args: P & { slide_id: SlideId }; result: SlideView };
  slide_move: { args: P & { slide_id: SlideId; after?: SlideId; section?: string }; result: null };
  slide_lock: { args: P & { slide_id: SlideId; element_id?: string; locked: boolean }; result: null };
  slide_preview_png: { args: P & { slide_id: SlideId; png_base64: string }; result: null };
  chart_compute: { args: P & { source: ChartSource; steps: TransformStep[] }; result: Table };
  code_read: { args: P & { path: string }; result: FileContent };
  code_write: { args: P & { path: string; content: string; expected_hash?: string }; result: WriteResult };
  deck_build: { args: P; result: TaskId };
  deck_switch_mode: { args: P & { to: DeckMode; via?: "revert" | "import" }; result: TaskId };
  render_fidelity: { args: P & { slides?: SlideId[] }; result: TaskId };
  check_run: { args: P & { slides?: SlideId[] }; result: Diagnostic[] };
  refs_list: { args: P; result: RefView[] };
  refs_add: { args: P & { paths_abs: string[] }; result: TaskId };
  refs_remove: { args: P & { ref_id: string }; result: null };
  refs_remove_unused: { args: P; result: string[] };
  refs_search: { args: P & { query: string; ref_ids?: string[] }; result: SearchHit[] };
  source_passage: { args: P & { cite: Cite }; result: SourcePassage };
  analyst_scan: { args: P & { repo_abs: string }; result: AnalystItemView[] };
  analyst_import: { args: P & { repo_abs: string; item: string }; result: RefView };
  assets_list: { args: P; result: AssetView[] };
  asset_add: { args: P & { path_abs: string }; result: AssetView };
  history_list: { args: P & { limit?: number }; result: SnapshotView[] };
  history_restore: { args: P & { snapshot_id: SnapshotId }; result: SnapshotView };
  history_revert_turn: { args: P & { turn: TurnId }; result: SnapshotView };
  exports_list: { args: P; result: ExportView[] };
  export_run: { args: P & { format: string; preset?: string; options?: Record<string, unknown> }; result: TaskId };
  export_reveal: { args: P & { export_id: string }; result: null };
  export_open: { args: P & { export_id: string }; result: null };
  conversation_new: { args: P; result: ConversationSummary };
  conversation_select: { args: P & { conversation: ConversationId }; result: null };
  conversation_transcript: { args: P & { conversation: ConversationId }; result: TranscriptEvent[] };
  chat_send: { args: P & { conversation: ConversationId; text: string; attachments?: Partial<ChatAttachments> }; result: TurnId };
  chat_cancel: { args: P & { conversation: ConversationId }; result: null };
  chat_set_mode: { args: P & { conversation: ConversationId; mode: Mode }; result: null };
  chat_set_model: { args: P & { conversation: ConversationId; model: string }; result: null };
  proposal_respond: { args: P & { proposal_id: ProposalId; decision: ProposalDecision }; result: null };
  skills_list: { args: P; result: SkillView[] };
  skill_set_enabled: { args: P & { name: string; enabled: boolean }; result: null };
  models_list: { args: Record<string, never>; result: ModelEntry[] };
  model_probe: { args: { model: string }; result: ModelEntry };
  provider_set_key: { args: { provider: string; key: string }; result: null };
  audit_export: { args: { from: string; to: string; path_abs: string }; result: null };
  dialog_pick_file: { args: DialogArgs; result: string | null };
  dialog_pick_files: { args: DialogArgs; result: string[] | null };
  dialog_pick_directory: { args: DialogArgs; result: string | null };
  dialog_save_file: { args: DialogArgs; result: string | null };
}

export type CommandName = keyof Commands;

/** Every command of IPC.md section 4, for exhaustiveness tests. */
export const COMMAND_NAMES = [
  "app_info",
  "projects_list",
  "project_create",
  "project_open",
  "project_close",
  "project_subscribe",
  "project_set_policy",
  "project_layout_get",
  "project_layout_set",
  "template_thumbnails",
  "template_replace",
  "deck_get",
  "slide_update",
  "slide_create",
  "slide_delete",
  "slide_duplicate",
  "slide_move",
  "slide_lock",
  "slide_preview_png",
  "chart_compute",
  "code_read",
  "code_write",
  "deck_build",
  "deck_switch_mode",
  "render_fidelity",
  "check_run",
  "refs_list",
  "refs_add",
  "refs_remove",
  "refs_remove_unused",
  "refs_search",
  "source_passage",
  "analyst_scan",
  "analyst_import",
  "assets_list",
  "asset_add",
  "history_list",
  "history_restore",
  "history_revert_turn",
  "exports_list",
  "export_run",
  "export_reveal",
  "export_open",
  "conversation_new",
  "conversation_select",
  "conversation_transcript",
  "chat_send",
  "chat_cancel",
  "chat_set_mode",
  "chat_set_model",
  "proposal_respond",
  "skills_list",
  "skill_set_enabled",
  "models_list",
  "model_probe",
  "provider_set_key",
  "audit_export",
  "dialog_pick_file",
  "dialog_pick_files",
  "dialog_pick_directory",
  "dialog_save_file",
] as const satisfies readonly CommandName[];

// Compile-time check that the list above is complete.
type Missing = Exclude<CommandName, (typeof COMMAND_NAMES)[number]>;
export const COMMANDS_COMPLETE: Missing extends never ? true : never = true;
