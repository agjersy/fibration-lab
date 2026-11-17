// Minimal chat state: enough to hold the events of IPC.md section 3 until the chat panel (work
// package D6) renders them. Shape follows Personal Analyst apps/desktop/src/state/chat.ts at f229e4d.
import { create } from "zustand";
import type { ConversationId, ExportView, ProjectId, ProposalId, ProposalView, TurnId, TurnSummary } from "../ipc";

export type ProposalStatus = "pending" | "accept" | "accept_modified" | "reject" | "auto";

export type ChatItem =
  | { kind: "user"; id: string; text: string }
  | { kind: "assistant"; id: string; turn: TurnId; text: string; streaming: boolean }
  | { kind: "tool"; id: string; turn: TurnId; name: string; arguments: unknown; result: { ok: boolean; summary: string; image?: string } | null }
  | { kind: "proposal"; id: string; proposal_id: ProposalId }
  | { kind: "summary"; id: string; turn: TurnId; summary: TurnSummary }
  | { kind: "error"; id: string; turn: TurnId; message: string };

export interface Usage {
  input_tokens: number;
  output_tokens: number;
  context_window: number;
  context_used: number;
}

export interface ConversationState {
  items: ChatItem[];
  busy: boolean;
  turn: TurnId | null;
  usage: Usage | null;
}

export interface ProposalRecord {
  project: ProjectId;
  proposal: ProposalView;
  status: ProposalStatus;
  by: string | null;
}

interface ChatState {
  conversations: Record<ConversationId, ConversationState>;
  proposals: Record<ProposalId, ProposalRecord>;
  buildOutput: Record<ProjectId, string>;
  exports: Record<ProjectId, ExportView[]>;
  ensure(conversation: ConversationId): void;
  addUser(conversation: ConversationId, text: string): void;
  turnStarted(conversation: ConversationId, turn: TurnId): void;
  appendText(conversation: ConversationId, turn: TurnId, delta: string): void;
  toolCall(conversation: ConversationId, turn: TurnId, id: string, name: string, args: unknown): void;
  toolResult(conversation: ConversationId, id: string, ok: boolean, summary: string, image?: string): void;
  addProposal(project: ProjectId, proposal: ProposalView): void;
  resolveProposal(proposal_id: ProposalId, status: string, by: string): void;
  turnDone(conversation: ConversationId, turn: TurnId, summary: TurnSummary): void;
  turnError(conversation: ConversationId, turn: TurnId, message: string): void;
  setUsage(conversation: ConversationId, usage: Usage): void;
  appendBuildOutput(project: ProjectId, chunk: string): void;
  setExports(project: ProjectId, exports: ExportView[]): void;
  addExport(project: ProjectId, exp: ExportView): void;
}

const EMPTY: ConversationState = { items: [], busy: false, turn: null, usage: null };

let counter = 0;
const nextId = (prefix: string) => `${prefix}${++counter}`;

function update(s: ChatState, conversation: ConversationId, fn: (c: ConversationState) => Partial<ConversationState>): Pick<ChatState, "conversations"> {
  const cur = s.conversations[conversation] ?? EMPTY;
  return { conversations: { ...s.conversations, [conversation]: { ...cur, ...fn(cur) } } };
}

const finishStreaming = (items: ChatItem[]): ChatItem[] => items.map((it) => (it.kind === "assistant" && it.streaming ? { ...it, streaming: false } : it));

export const useChat = create<ChatState>((set) => ({
  conversations: {},
  proposals: {},
  buildOutput: {},
  exports: {},
  ensure: (conversation) => set((s) => (s.conversations[conversation] ? s : update(s, conversation, () => ({})))),
  addUser: (conversation, text) => set((s) => update(s, conversation, (c) => ({ items: [...c.items, { kind: "user", id: nextId("u"), text }] }))),
  turnStarted: (conversation, turn) => set((s) => update(s, conversation, () => ({ busy: true, turn }))),
  appendText: (conversation, turn, delta) =>
    set((s) =>
      update(s, conversation, (c) => {
        const last = c.items[c.items.length - 1];
        if (last && last.kind === "assistant" && last.streaming && last.turn === turn) return { items: [...c.items.slice(0, -1), { ...last, text: last.text + delta }] };
        return { items: [...c.items, { kind: "assistant", id: nextId("a"), turn, text: delta, streaming: true }] };
      }),
    ),
  toolCall: (conversation, turn, id, name, args) => set((s) => update(s, conversation, (c) => ({ items: [...finishStreaming(c.items), { kind: "tool", id, turn, name, arguments: args, result: null }] }))),
  toolResult: (conversation, id, ok, summary, image) =>
    set((s) => update(s, conversation, (c) => ({ items: c.items.map((it) => (it.kind === "tool" && it.id === id ? { ...it, result: image ? { ok, summary, image } : { ok, summary } } : it)) }))),
  addProposal: (project, proposal) =>
    set((s) => ({
      proposals: { ...s.proposals, [proposal.id]: { project, proposal, status: "pending", by: null } },
      ...update(s, proposal.conversation, (c) => ({ items: [...finishStreaming(c.items), { kind: "proposal", id: nextId("p"), proposal_id: proposal.id }] })),
    })),
  resolveProposal: (proposal_id, status, by) =>
    set((s) => {
      const rec = s.proposals[proposal_id];
      return rec ? { proposals: { ...s.proposals, [proposal_id]: { ...rec, status: status as ProposalStatus, by } } } : s;
    }),
  turnDone: (conversation, turn, summary) => set((s) => update(s, conversation, (c) => ({ busy: false, turn: null, items: [...finishStreaming(c.items), { kind: "summary", id: nextId("s"), turn, summary }] }))),
  turnError: (conversation, turn, message) => set((s) => update(s, conversation, (c) => ({ busy: false, turn: null, items: [...finishStreaming(c.items), { kind: "error", id: nextId("e"), turn, message }] }))),
  setUsage: (conversation, usage) => set((s) => update(s, conversation, () => ({ usage }))),
  appendBuildOutput: (project, chunk) => set((s) => ({ buildOutput: { ...s.buildOutput, [project]: ((s.buildOutput[project] ?? "") + chunk).slice(-200_000) } })),
  setExports: (project, exports) => set((s) => ({ exports: { ...s.exports, [project]: exports } })),
  addExport: (project, exp) => set((s) => ({ exports: { ...s.exports, [project]: [exp, ...(s.exports[project] ?? []).filter((e) => e.id !== exp.id)] } })),
}));
