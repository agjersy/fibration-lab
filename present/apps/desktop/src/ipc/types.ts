// IPC types are generated from crates/protocol (deck, template, diagnostics, and the UI types of
// `ui.rs`) by ts-rs (`cargo test -p present-protocol`) and re-exported unchanged. docs/IPC.md is the
// contract. The few types below are the ones the generated export does not carry.
import type { ProjectInfo } from "./generated";

export * from "./generated";
export type { TurnSummaryView as TurnSummary, Thumb as SlideThumb } from "./generated";

export type ProjectId = string;
export type ConversationId = string;
export type TurnId = string;
export type ProposalId = string;
export type TaskId = string;

/** `ProjectSummary` of IPC.md section 2; the generated `ProjectInfo` flattens it. */
export type ProjectSummary = Pick<ProjectInfo, "id" | "name" | "path_abs" | "template_name" | "deck_mode" | "slides" | "model" | "created_at" | "last_opened_at" | "open">;

/** IPC.md section 2 `AnalystItem` (the UI view; the kernel's own `AnalystItem` in ./generated differs). */
export interface AnalystItemView {
  id: string;
  kind: "figure" | "table";
  title: string;
  run_id: string;
  script: string;
  args: string[];
  created: string;
  thumb: string | null;
}

export interface DialogFilter {
  name: string;
  extensions: string[];
}
