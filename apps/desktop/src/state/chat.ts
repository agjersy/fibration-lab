import { create } from "zustand";
import type { Attachment, ConversationId, ProposalId, ProposalView, RunId, SessionId, TurnId, TurnSummary } from "../ipc";
import { frameBatcher } from "./frame";

/** "expired": recorded in the transcript without a decision and no longer pending in the backend. */
export type ProposalStatus = "pending" | "accept" | "reject" | "auto" | "expired";

export type ChatItem =
  | { kind: "user"; id: string; text: string; attachments: Attachment[] }
  /** While `streaming`, the text so far is in `ConversationState.stream`, not here. */
  | { kind: "assistant"; id: string; turn: TurnId; text: string; streaming: boolean }
  | { kind: "tool"; id: string; turn: TurnId; name: string; arguments: unknown; result: { ok: boolean; summary: string } | null }
  | { kind: "proposal"; id: string; proposal_id: ProposalId }
  | { kind: "run"; id: string; run_id: RunId }
  | { kind: "summary"; id: string; turn: TurnId; summary: TurnSummary }
  | { kind: "error"; id: string; turn: TurnId; message: string }
  /** An accepted refresh from analysis that could not be written (the slide changed meanwhile). */
  | { kind: "refresh_failed"; id: string; proposal_id: ProposalId; deck: string; slide_id: string; message: string };

export interface Usage {
  input_tokens: number;
  output_tokens: number;
  context_window: number;
  context_used: number;
}

/** Text of the assistant message being streamed; `id` names its item in `items`. */
export interface StreamSlot {
  id: string;
  turn: TurnId;
  text: string;
}

export interface ConversationState {
  items: ChatItem[];
  /**
   * The message currently streaming. Deltas only replace this slot, so `items`
   * keeps its identity while text arrives; the text moves into the item when
   * the message ends (the next item, the end of the turn).
   */
  stream: StreamSlot | null;
  busy: boolean;
  turn: TurnId | null;
  usage: Usage | null;
  /** The stored transcript has been mapped into `items`. */
  loaded: boolean;
}

export interface ProposalRecord {
  session: SessionId;
  proposal: ProposalView;
  status: ProposalStatus;
  by: "user" | "policy" | null;
}

interface ChatState {
  conversations: Record<ConversationId, ConversationState>;
  proposals: Record<ProposalId, ProposalRecord>;
  drafts: Record<ConversationId, string>;
  pendingAttachments: Record<ConversationId, Attachment[]>;
  /** Proposals with a `proposal_respond` call in flight. */
  responding: Record<ProposalId, boolean>;
  ensure(conversation: ConversationId): void;
  /**
   * Installs items mapped from the stored transcript. Items that streamed in
   * while it was being fetched are kept after it unless `replace` is set (resync).
   */
  loadHistory(conversation: ConversationId, items: ChatItem[], proposals: ProposalRecord[], replace: boolean): void;
  /** Adds the optimistic user message and returns its item id so a failed send can take it back. */
  addUser(conversation: ConversationId, text: string, attachments: Attachment[]): string;
  removeItem(conversation: ConversationId, id: string): void;
  /**
   * Reconciles with `SessionInfo.pending_proposals`. Unknown proposals get a
   * card; with `authoritative`, proposals this session still shows as pending
   * but the backend no longer lists are marked expired.
   */
  syncPending(session: SessionId, pending: ProposalView[], authoritative: boolean): void;
  setResponding(proposal_id: ProposalId, responding: boolean): void;
  forget(conversations: ConversationId[]): void;
  turnStarted(conversation: ConversationId, turn: TurnId): void;
  /** Appends streamed text. Events go through `queueAgentText`, which calls this once per frame. */
  appendText(conversation: ConversationId, turn: TurnId, delta: string): void;
  toolCall(conversation: ConversationId, turn: TurnId, id: string, name: string, args: unknown): void;
  toolResult(conversation: ConversationId, id: string, ok: boolean, summary: string): void;
  addProposal(session: SessionId, proposal: ProposalView): void;
  resolveProposal(proposal_id: ProposalId, status: Exclude<ProposalStatus, "pending">, by: "user" | "policy" | null): void;
  addRun(conversation: ConversationId, run_id: RunId): void;
  turnDone(conversation: ConversationId, turn: TurnId, summary: TurnSummary): void;
  turnError(conversation: ConversationId, turn: TurnId, message: string): void;
  refreshFailed(conversation: ConversationId, proposal_id: ProposalId, deck: string, slide_id: string, message: string): void;
  setUsage(conversation: ConversationId, usage: Usage): void;
  setBusy(conversation: ConversationId, busy: boolean): void;
  setDraft(conversation: ConversationId, text: string): void;
  addAttachment(conversation: ConversationId, attachment: Attachment): void;
  clearAttachments(conversation: ConversationId): void;
  removeAttachment(conversation: ConversationId, index: number): void;
}

const EMPTY: ConversationState = { items: [], stream: null, busy: false, turn: null, usage: null, loaded: false };

let counter = 0;
function nextId(prefix: string): string {
  return `${prefix}${++counter}`;
}

function update(
  s: ChatState,
  conversation: ConversationId,
  fn: (c: ConversationState) => Partial<ConversationState>,
): Pick<ChatState, "conversations"> {
  const cur = s.conversations[conversation] ?? EMPTY;
  return { conversations: { ...s.conversations, [conversation]: { ...cur, ...fn(cur) } } };
}

/** The items with the streaming message (if any) completed: its text moved in from the slot. */
function committed(c: ConversationState): ChatItem[] {
  const stream = c.stream;
  if (!stream) return c.items;
  return c.items.map((it) => (it.kind === "assistant" && it.id === stream.id ? { ...it, text: stream.text, streaming: false } : it));
}

/** Ends the streaming message and appends `item` after it. */
function append(c: ConversationState, item: ChatItem): Pick<ConversationState, "items" | "stream"> {
  return { items: [...committed(c), item], stream: null };
}

export const useChat = create<ChatState>((set) => ({
  conversations: {},
  proposals: {},
  drafts: {},
  pendingAttachments: {},
  responding: {},
  ensure: (conversation) => set((s) => (s.conversations[conversation] ? {} : update(s, conversation, () => ({})))),
  loadHistory: (conversation, items, proposals, replace) =>
    set((s) => {
      const known = new Set(items.map((it) => it.id));
      const cur = s.conversations[conversation];
      const live = replace ? [] : (cur?.items ?? []).filter((it) => !known.has(it.id));
      const records = { ...s.proposals };
      for (const rec of proposals) {
        const cur = records[rec.proposal.id];
        // A record that came in live carries the full ProposalView and the
        // freshest status; history only fills in what is not known yet.
        if (!cur || replace || cur.status === "pending") records[rec.proposal.id] = cur && cur.proposal.proposed !== null && rec.proposal.proposed === null ? { ...rec, proposal: cur.proposal } : rec;
      }
      // A replaced conversation has no item left for the slot to belong to.
      return { proposals: records, ...update(s, conversation, (c) => ({ items: [...items, ...live], stream: replace ? null : c.stream, loaded: true })) };
    }),
  addUser: (conversation, text, attachments) => {
    const id = nextId("u");
    set((s) => update(s, conversation, (c) => ({ ...append(c, { kind: "user", id, text, attachments }), busy: true })));
    return id;
  },
  removeItem: (conversation, id) =>
    set((s) => update(s, conversation, (c) => ({ items: c.items.filter((it) => it.id !== id), stream: c.stream?.id === id ? null : c.stream }))),
  syncPending: (session, pending, authoritative) =>
    set((s) => {
      let next: Pick<ChatState, "conversations"> & { proposals: ChatState["proposals"] } = { conversations: s.conversations, proposals: s.proposals };
      const listed = new Set(pending.map((p) => p.id));
      for (const proposal of pending) {
        const cur = next.proposals[proposal.id];
        if (cur && cur.status !== "expired") continue;
        const proposals = { ...next.proposals, [proposal.id]: { session, proposal, status: "pending" as const, by: null } };
        const conv = next.conversations[proposal.conversation] ?? EMPTY;
        const id = `proposal:${proposal.id}`;
        const changed = conv.items.some((it) => it.id === id) ? {} : append(conv, { kind: "proposal", id, proposal_id: proposal.id });
        next = { proposals, conversations: { ...next.conversations, [proposal.conversation]: { ...conv, ...changed } } };
      }
      if (authoritative) {
        const proposals = { ...next.proposals };
        let changed = false;
        for (const rec of Object.values(proposals)) {
          if (rec.session === session && rec.status === "pending" && !listed.has(rec.proposal.id)) {
            proposals[rec.proposal.id] = { ...rec, status: "expired", by: null };
            changed = true;
          }
        }
        if (changed) next = { ...next, proposals };
      }
      return next;
    }),
  setResponding: (proposal_id, responding) =>
    set((s) => {
      const next = { ...s.responding };
      if (responding) next[proposal_id] = true;
      else delete next[proposal_id];
      return { responding: next };
    }),
  forget: (conversations) =>
    set((s) => {
      const next = { ...s.conversations };
      for (const id of conversations) delete next[id];
      const gone = new Set(conversations);
      const proposals = Object.fromEntries(Object.entries(s.proposals).filter(([, rec]) => !gone.has(rec.proposal.conversation)));
      return { conversations: next, proposals };
    }),
  turnStarted: (conversation, turn) => set((s) => update(s, conversation, () => ({ busy: true, turn }))),
  appendText: (conversation, turn, delta) =>
    set((s) =>
      update(s, conversation, (c) => {
        // Same message: only the slot changes; `items` is not touched.
        if (c.stream && c.stream.turn === turn) return { stream: { ...c.stream, text: c.stream.text + delta } };
        const id = nextId("a");
        return { ...append(c, { kind: "assistant", id, turn, text: "", streaming: true }), stream: { id, turn, text: delta } };
      }),
    ),
  toolCall: (conversation, turn, id, name, args) =>
    set((s) =>
      update(s, conversation, (c) =>
        // Already present when the transcript was loaded while the call was running.
        c.items.some((it) => it.id === `tool:${id}`) ? {} : append(c, { kind: "tool", id: `tool:${id}`, turn, name, arguments: args, result: null }),
      ),
    ),
  toolResult: (conversation, id, ok, summary) =>
    set((s) =>
      update(s, conversation, (c) => ({
        items: c.items.map((it) => (it.kind === "tool" && it.id === `tool:${id}` ? { ...it, result: { ok, summary } } : it)),
      })),
    ),
  addProposal: (session, proposal) =>
    set((s) => ({
      proposals: { ...s.proposals, [proposal.id]: { session, proposal, status: "pending", by: null } },
      ...update(s, proposal.conversation, (c) => {
        const id = `proposal:${proposal.id}`;
        return c.items.some((it) => it.id === id) ? {} : append(c, { kind: "proposal", id, proposal_id: proposal.id });
      }),
    })),
  resolveProposal: (proposal_id, status, by) =>
    set((s) => {
      const rec = s.proposals[proposal_id];
      if (!rec) return {};
      return { proposals: { ...s.proposals, [proposal_id]: { ...rec, status, by } } };
    }),
  addRun: (conversation, run_id) =>
    set((s) =>
      update(s, conversation, (c) => (c.items.some((it) => it.kind === "run" && it.run_id === run_id) ? {} : append(c, { kind: "run", id: `run:${run_id}`, run_id }))),
    ),
  turnDone: (conversation, turn, summary) =>
    set((s) => update(s, conversation, (c) => ({ ...append(c, { kind: "summary", id: nextId("s"), turn, summary }), busy: false, turn: null }))),
  turnError: (conversation, turn, message) =>
    set((s) => update(s, conversation, (c) => ({ ...append(c, { kind: "error", id: nextId("e"), turn, message }), busy: false, turn: null }))),
  refreshFailed: (conversation, proposal_id, deck, slide_id, message) =>
    set((s) =>
      update(s, conversation, (c) => {
        const id = `refresh-failed:${proposal_id}:${nextId("r")}`;
        return append(c, { kind: "refresh_failed", id, proposal_id, deck, slide_id, message });
      }),
    ),
  setUsage: (conversation, usage) => set((s) => update(s, conversation, () => ({ usage }))),
  setBusy: (conversation, busy) => set((s) => update(s, conversation, () => ({ busy }))),
  setDraft: (conversation, text) => set((s) => ({ drafts: { ...s.drafts, [conversation]: text } })),
  addAttachment: (conversation, attachment) =>
    set((s) => ({ pendingAttachments: { ...s.pendingAttachments, [conversation]: [...(s.pendingAttachments[conversation] ?? []), attachment] } })),
  clearAttachments: (conversation) => set((s) => ({ pendingAttachments: { ...s.pendingAttachments, [conversation]: [] } })),
  removeAttachment: (conversation, index) =>
    set((s) => ({
      pendingAttachments: { ...s.pendingAttachments, [conversation]: (s.pendingAttachments[conversation] ?? []).filter((_, i) => i !== index) },
    })),
}));

export function useConversation(conversation: ConversationId | null): ConversationState {
  return useChat((s) => (conversation ? (s.conversations[conversation] ?? EMPTY) : EMPTY));
}

/** The item list alone: its identity does not change while a message streams. */
export function useConversationItems(conversation: ConversationId | null): ChatItem[] {
  return useChat((s) => (conversation ? (s.conversations[conversation] ?? EMPTY).items : EMPTY.items));
}

/** Text of a streaming message, or null once it has ended (the item then carries the text). */
export function useStreamText(conversation: ConversationId, id: string): string | null {
  return useChat((s) => {
    const stream = s.conversations[conversation]?.stream;
    return stream && stream.id === id ? stream.text : null;
  });
}

export function useProposal(proposal_id: ProposalId | null): ProposalRecord | undefined {
  return useChat((s) => (proposal_id ? s.proposals[proposal_id] : undefined));
}

export function useResponding(proposal_id: ProposalId | null): boolean {
  return useChat((s) => (proposal_id ? s.responding[proposal_id] === true : false));
}

// ---- streamed text batching ----
// `agent_text` deltas arrive per token. They are collected here and applied
// once per animation frame, so a long answer costs one store update (and one
// render of the streaming message) per frame, whatever the token rate.

const pendingText = new Map<ConversationId, { turn: TurnId; parts: string[] }[]>();
const textFrame = frameBatcher(() => flushAgentText());

export function queueAgentText(conversation: ConversationId, turn: TurnId, delta: string): void {
  let runs = pendingText.get(conversation);
  if (!runs) {
    runs = [];
    pendingText.set(conversation, runs);
  }
  const last = runs[runs.length - 1];
  if (last && last.turn === turn) last.parts.push(delta);
  else runs.push({ turn, parts: [delta] });
  textFrame.schedule();
}

/**
 * Applies the queued deltas. Called per frame, and by the dispatcher before
 * any other event so that text and the items around it keep their order.
 */
export function flushAgentText(): void {
  textFrame.cancel();
  if (pendingText.size === 0) return;
  const batch = [...pendingText];
  pendingText.clear();
  const chat = useChat.getState();
  for (const [conversation, runs] of batch) for (const run of runs) chat.appendText(conversation, run.turn, run.parts.join(""));
}
