// Maps a stored transcript (PROTOCOL.md section 5) onto the chat items the
// panel renders, so a conversation looks the same after a restart as it did live.
import type { ConversationId, ProposalView, SessionId, TranscriptEvent } from "../ipc";
import type { ChatItem, ProposalRecord } from "./chat";

export interface MappedTranscript {
  items: ChatItem[];
  proposals: ProposalRecord[];
}

function resultSummary(result: unknown, truncated: boolean | undefined): string {
  let text: string;
  if (typeof result === "string") text = result;
  else {
    try {
      text = JSON.stringify(result, null, 2) ?? "";
    } catch {
      text = String(result);
    }
  }
  if (text.length > 4000) text = `${text.slice(0, 4000)}…`;
  return truncated ? `${text}\n[truncated]` : text;
}

/**
 * `pending` is `SessionInfo.pending_proposals`: a proposal the transcript
 * records without a decision is live only if the backend still lists it;
 * otherwise its turn ended without one and the card is shown as expired.
 */
export function transcriptToChat(session: SessionId, conversation: ConversationId, events: readonly TranscriptEvent[], pending: readonly ProposalView[]): MappedTranscript {
  const items: ChatItem[] = [];
  const proposals = new Map<string, ProposalRecord>();
  const tools = new Map<string, number>();
  const live = new Map(pending.filter((p) => p.conversation === conversation).map((p) => [p.id, p]));
  const turn = `history:${conversation}`;

  events.forEach((e, i) => {
    switch (e.type) {
      case "user":
        items.push({ kind: "user", id: `h${i}`, text: e.text, attachments: (e.attachments ?? []).map((path) => ({ kind: "file", path })) });
        break;
      case "assistant":
        if (e.text.trim() !== "") items.push({ kind: "assistant", id: `h${i}`, turn, text: e.text, streaming: false });
        break;
      case "tool_call":
        tools.set(e.id, items.length);
        items.push({ kind: "tool", id: `tool:${e.id}`, turn, name: e.name, arguments: e.arguments, result: null });
        break;
      case "tool_result": {
        const at = tools.get(e.id);
        const item = at === undefined ? undefined : items[at];
        if (at !== undefined && item && item.kind === "tool") items[at] = { ...item, result: { ok: e.ok, summary: resultSummary(e.result, e.truncated) } };
        break;
      }
      case "proposal": {
        const view: ProposalView = live.get(e.id) ?? {
          id: e.id,
          conversation,
          kind: e.kind,
          path: null,
          summary: e.summary,
          original: null,
          proposed: null,
          diff: e.diff ?? null,
          command: null,
          packages: null,
          message: null,
          in_scope: true,
          reason: "review",
        };
        proposals.set(e.id, { session, proposal: view, status: live.has(e.id) ? "pending" : "expired", by: null });
        items.push({ kind: "proposal", id: `proposal:${e.id}`, proposal_id: e.id });
        break;
      }
      case "decision": {
        const rec = proposals.get(e.proposal_id);
        if (rec) proposals.set(e.proposal_id, { ...rec, status: e.decision, by: e.by });
        break;
      }
      case "run":
        if (!items.some((it) => it.kind === "run" && it.run_id === e.run_id)) items.push({ kind: "run", id: `run:${e.run_id}`, run_id: e.run_id });
        break;
      case "summary":
        items.push({ kind: "summary", id: `h${i}`, turn, summary: { text: e.text, files_changed: e.files_changed, runs: e.runs, figures: e.figures } });
        break;
      case "checkpoint":
      case "usage":
        break;
    }
  });

  // Pending proposals the transcript does not mention yet still get a card.
  for (const p of live.values()) {
    if (proposals.has(p.id)) continue;
    proposals.set(p.id, { session, proposal: p, status: "pending", by: null });
    items.push({ kind: "proposal", id: `proposal:${p.id}`, proposal_id: p.id });
  }
  return { items, proposals: [...proposals.values()] };
}
