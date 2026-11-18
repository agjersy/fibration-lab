// Generated from the command table in crates/core/src/commands.rs by
// `cargo test -p analyst-core --lib commands` (do not edit; a test fails when it is stale).
// Argument names are exact (snake_case); see docs/IPC.md sections 4, 7, 9 and 10.
import type {
  AppInfo,
  ArtifactEntry,
  ArtifactKind,
  Attachment,
  CheckpointView,
  ConversationSummary,
  Deck,
  DeckSummary,
  Diagnostic,
  EqCheck,
  Equation,
  FigureData,
  FigureGroup,
  FileContent,
  FileEntry,
  IndexResult,
  ModelEntry,
  OutListResult,
  OutReadResult,
  PackageInfo,
  PolicyPatch,
  PolicyView,
  ProposalDecision,
  ProposalView,
  ProviderKeyStatus,
  RunDetail,
  RunView,
  SearchHit,
  SessionInfo,
  SessionSummary,
  TargetInfo,
  TerminalOpened,
  TranscriptEvent,
  VerificationView,
  VerifyItem,
  VerifyState,
  WriteResult,
} from "./types";

type S = { session: string };
/** A deck command: the session and the deck's slug under `decks/`. */
type D = S & { deck: string };

export interface Commands {
  session_subscribe: { args: S & { channel: unknown }; result: string };
  session_unsubscribe: { args: { subscription: string }; result: null };
  app_info: { args: Record<string, never>; result: AppInfo };
  sessions_list: { args: Record<string, never>; result: SessionSummary[] };
  session_create: { args: { repo_abs: string; target?: string; model?: string; init?: boolean }; result: SessionInfo };
  session_open: { args: S; result: SessionInfo };
  session_close: { args: S; result: null };
  session_info: { args: S; result: SessionInfo };
  session_rename: { args: S & { name: string }; result: SessionSummary };
  session_set_target: { args: S & { target: string }; result: string };
  session_set_policy: { args: S & { policy: PolicyPatch }; result: PolicyView };
  layout_get: { args: S; result: string | null };
  layout_save: { args: S & { layout: string }; result: null };
  fs_tree: { args: S & { path?: string; depth?: number }; result: FileEntry[] };
  fs_read: { args: S & { path: string }; result: FileContent };
  fs_read_binary: { args: S & { path: string }; result: FigureData };
  fs_write: { args: S & { path: string; content: string; expected_hash?: string }; result: WriteResult };
  fs_create: { args: S & { path: string; is_dir: boolean }; result: null };
  fs_delete: { args: S & { path: string }; result: null };
  fs_rename: { args: S & { from: string; to: string }; result: null };
  fs_search: { args: S & { pattern: string; glob?: string; max_results?: number }; result: SearchHit[] };
  index_get: { args: S; result: IndexResult };
  check_run: { args: S & { paths?: string[] }; result: Diagnostic[] };
  run_start: { args: S & { script: string; args: string[] }; result: RunView };
  run_cancel: { args: S & { run_id: string }; result: null };
  runs_list: { args: S & { limit?: number }; result: RunView[] };
  run_get: { args: S & { run_id: string }; result: RunDetail };
  out_list: { args: S & { path?: string; run_id?: string }; result: OutListResult };
  out_read: { args: S & { path: string; max_bytes?: number }; result: OutReadResult };
  figures_list: { args: S & { limit?: number }; result: FigureGroup[] };
  figure_data: { args: S & { path: string }; result: FigureData };
  eq_extract: { args: S & { path: string }; result: Equation[] };
  eq_check: { args: S & { path: string; function: string }; result: EqCheck };
  env_packages: { args: S; result: PackageInfo[] };
  env_add: { args: S & { packages: string[] }; result: string };
  env_remove: { args: S & { packages: string[] }; result: string };
  env_sync: { args: S; result: string };
  conversation_new: { args: S & { title?: string }; result: ConversationSummary };
  conversation_select: { args: S & { conversation: string }; result: null };
  conversation_transcript: { args: S & { conversation: string }; result: TranscriptEvent[] };
  chat_send: { args: S & { conversation: string; text: string; attachments?: Attachment[] }; result: string };
  chat_cancel: { args: S & { conversation: string }; result: null };
  chat_set_mode: { args: S & { conversation: string; mode: string }; result: null };
  chat_set_model: { args: S & { conversation: string; model: string }; result: null };
  proposal_respond: { args: S & { proposal_id: string; decision: ProposalDecision }; result: null };
  models_list: { args: Record<string, never>; result: ModelEntry[] };
  model_probe: { args: { model: string }; result: ModelEntry };
  provider_keys_status: { args: Record<string, never>; result: ProviderKeyStatus[] };
  provider_key_set: { args: { provider: string; key: string }; result: null };
  targets_list: { args: Record<string, never>; result: TargetInfo[] };
  target_bootstrap: { args: { target: string }; result: string };
  terminal_open: { args: S & { cols: number; rows: number }; result: TerminalOpened };
  terminal_write: { args: { terminal_id: string; data: string }; result: null };
  terminal_resize: { args: { terminal_id: string; cols: number; rows: number }; result: null };
  terminal_close: { args: { terminal_id: string }; result: null };
  dialog_pick_directory: { args: { title?: string }; result: string | null };
  dialog_pick_files: { args: { title?: string; filters?: { name: string; extensions: string[] }[]; multiple?: boolean }; result: string[] | null };
  decks_list: { args: S; result: DeckSummary[] };
  deck_create: { args: S & { title: string; template_abs: string; slug?: string; code?: boolean }; result: DeckSummary };
  deck_open: { args: D; result: Deck.DeckInfo };
  template_thumbnails: { args: D; result: Deck.LayoutThumbnail[] };
  template_replace: { args: D & { template_abs: string }; result: string };
  deck_get: { args: D; result: Deck.DeckView };
  slide_update: { args: D & { slide: Deck.Slide; expected_hash?: string }; result: Deck.SlideView };
  slide_create: { args: D & { after?: string; layout: string }; result: Deck.SlideView };
  slide_delete: { args: D & { slide_id: string }; result: null };
  slide_duplicate: { args: D & { slide_id: string }; result: Deck.SlideView };
  slide_move: { args: D & { slide_id: string; after?: string; section?: string }; result: null };
  slide_lock: { args: D & { slide_id: string; element_id?: string; locked: boolean }; result: null };
  slide_place_figure: { args: D & { slide_id: string; run_id: string; artifact: string; placeholder?: string }; result: Deck.SlideView };
  slide_place_equation: { args: D & { slide_id: string; path: string; function: string; placeholder?: string }; result: Deck.SlideView };
  slide_preview_png: { args: D & { slide_id: string; png_base64: string }; result: null };
  chart_compute: { args: D & { source: unknown; steps?: unknown }; result: Deck.ChartComputeResult };
  deck_code_read: { args: D & { path: string }; result: Deck.FileContent };
  deck_code_write: { args: D & { path: string; content: string; expected_hash?: string }; result: Deck.WriteResult };
  deck_build: { args: D; result: string };
  deck_switch_mode: { args: D & { to: Deck.DeckMode; via?: string }; result: string };
  render_fidelity: { args: D & { slides?: string[] }; result: string };
  deck_check_run: { args: D & { slides?: string[] }; result: Deck.Diagnostic[] };
  refs_list: { args: D; result: Deck.RefView[] };
  refs_add: { args: D & { paths_abs: string[] }; result: string };
  refs_remove: { args: D & { ref_id: string }; result: null };
  refs_remove_unused: { args: D; result: string[] };
  refs_search: { args: D & { query: string; ref_ids?: string[] }; result: Deck.SearchHit[] };
  source_passage: { args: D & { cite: string }; result: Deck.SourcePassage };
  assets_list: { args: D; result: Deck.AssetView[] };
  asset_add: { args: D & { path_abs: string }; result: Deck.AssetView };
  exports_list: { args: D; result: Deck.ExportView[] };
  export_run: { args: D & { format: string }; result: string };
  export_reveal: { args: D & { export_id: string }; result: null };
  export_open: { args: D & { export_id: string }; result: null };
  history_list: { args: S & { limit?: number }; result: CheckpointView[] };
  history_restore: { args: S & { checkpoint: string }; result: null };
  history_revert_turn: { args: S & { turn: string }; result: CheckpointView };
  artifacts_list: { args: S & { run_id?: string; kind?: ArtifactKind; limit?: number }; result: ArtifactEntry[] };
  verification_list: { args: S; result: VerificationView[] };
  verification_set: { args: S & { item: VerifyItem; state: VerifyState; note?: string }; result: VerificationView };
  deck_verification_gaps: { args: D; result: VerificationView[] };
  slide_refresh: { args: D & { slide_id: string; element_id?: string }; result: ProposalView | null };
  deck_refresh_stale: { args: D; result: ProposalView[] };
}

export type CommandName = keyof Commands;
