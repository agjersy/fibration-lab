import { beforeEach, describe, expect, it } from "vitest";
import type { ProposalView, TranscriptEvent } from "../ipc";
import { resetStores } from "../test/util";
import { useChat } from "./chat";
import { transcriptToChat } from "./transcript";

const S = "s1";
const C = "c1";
const ts = "2026-11-01T09:00:00Z";

function view(id: string, over: Partial<ProposalView> = {}): ProposalView {
  return {
    id,
    conversation: C,
    kind: "edit",
    path: "analysis/models.py",
    summary: "edit models",
    original: "a\n",
    proposed: "b\n",
    diff: "-a\n+b\n",
    command: null,
    packages: null,
    message: null,
    in_scope: true,
    reason: "review",
    ...over,
  };
}

const events: TranscriptEvent[] = [
  { ts, type: "user", text: "Fit the decay", attachments: ["data/sample.csv"] },
  { ts, type: "assistant", text: "Looking at the library. " },
  { ts, type: "tool_call", id: "t1", name: "lib.index", arguments: {} },
  { ts, type: "tool_result", id: "t1", ok: true, result: { functions: 2 } },
  { ts, type: "proposal", id: "p1", kind: "edit", summary: "edit models", diff: "-a\n+b\n" },
  { ts, type: "decision", proposal_id: "p1", decision: "accept", by: "user" },
  { ts, type: "checkpoint", ref: "refs/fibration/1" },
  { ts, type: "tool_call", id: "t2", name: "run.script", arguments: { script: "scripts/fit.py" } },
  { ts, type: "run", run_id: "r1", script: "scripts/fit.py", args: [], target: "local" },
  { ts, type: "tool_result", id: "t2", ok: false, result: "exit 1", truncated: true },
  { ts, type: "assistant", text: "   " },
  { ts, type: "summary", text: "Edited and ran.", files_changed: ["analysis/models.py"], runs: ["r1"], figures: [] },
  { ts, type: "usage", input_tokens: 1, output_tokens: 1, model: "m" },
  { ts, type: "proposal", id: "p2", kind: "shell", summary: "run make" },
  { ts, type: "proposal", id: "p3", kind: "edit", summary: "second edit" },
];

describe("transcriptToChat", () => {
  it("maps every transcript event kind onto chat items", () => {
    const { items, proposals } = transcriptToChat(S, C, events, [view("p3")]);
    expect(items.map((i) => i.kind)).toEqual(["user", "assistant", "tool", "proposal", "tool", "run", "summary", "proposal", "proposal"]);
    expect(items[0]).toMatchObject({ kind: "user", text: "Fit the decay", attachments: [{ kind: "file", path: "data/sample.csv" }] });
    expect(items[1]).toMatchObject({ kind: "assistant", streaming: false });
    expect(items[2]).toMatchObject({ kind: "tool", id: "tool:t1", name: "lib.index", result: { ok: true } });
    expect(items[4]).toMatchObject({ kind: "tool", result: { ok: false, summary: "exit 1\n[truncated]" } });
    expect(items[5]).toMatchObject({ kind: "run", run_id: "r1" });
    expect(items[6]).toMatchObject({ kind: "summary", summary: { text: "Edited and ran.", runs: ["r1"] } });
    const by = Object.fromEntries(proposals.map((p) => [p.proposal.id, p]));
    expect(by.p1).toMatchObject({ status: "accept", by: "user", session: S, proposal: { diff: "-a\n+b\n", proposed: null } });
    // Undecided and not pending in the backend any more: its turn ended without a decision.
    expect(by.p2).toMatchObject({ status: "expired", proposal: { kind: "shell" } });
    // Undecided and still pending: a live card with the full view from SessionInfo.
    expect(by.p3).toMatchObject({ status: "pending", proposal: { proposed: "b\n", path: "analysis/models.py" } });
  });

  it("adds a card for a pending proposal the transcript does not mention, and ignores other conversations", () => {
    const { items, proposals } = transcriptToChat(S, C, [], [view("p9"), view("other", { conversation: "c2" })]);
    expect(items).toEqual([{ kind: "proposal", id: "proposal:p9", proposal_id: "p9" }]);
    expect(proposals.map((p) => p.proposal.id)).toEqual(["p9"]);
  });

  it("item ids are stable, so loading twice does not duplicate", () => {
    const a = transcriptToChat(S, C, events, []);
    const b = transcriptToChat(S, C, events, []);
    expect(a.items.map((i) => i.id)).toEqual(b.items.map((i) => i.id));
    expect(new Set(a.items.map((i) => i.id)).size).toBe(a.items.length);
  });
});

describe("chat store with history", () => {
  beforeEach(resetStores);

  it("keeps items that streamed in while the transcript was loading, without duplicating known ones", () => {
    const chat = useChat.getState();
    // Live events arrive first: a tool call that is also in the transcript, and new text.
    chat.toolCall(C, "turn1", "t2", "run.script", {});
    chat.appendText(C, "turn1", "still working");
    const mapped = transcriptToChat(S, C, events.slice(0, 8), []);
    chat.loadHistory(C, mapped.items, mapped.proposals, false);
    const conv = useChat.getState().conversations[C];
    expect(conv?.loaded).toBe(true);
    expect(conv?.items.filter((i) => i.id === "tool:t2")).toHaveLength(1);
    expect(conv?.items.at(-1)).toMatchObject({ kind: "assistant", streaming: true });
    expect(conv?.stream).toMatchObject({ id: conv?.items.at(-1)?.id, text: "still working" });
    // A resync replaces everything.
    chat.loadHistory(C, mapped.items, mapped.proposals, true);
    expect(useChat.getState().conversations[C]?.items).toHaveLength(mapped.items.length);
    expect(useChat.getState().conversations[C]?.stream).toBeNull();
  });

  it("syncPending adds live cards and, when authoritative, expires the ones the backend dropped", () => {
    const chat = useChat.getState();
    chat.syncPending(S, [view("p1"), view("p2")], true);
    expect(useChat.getState().conversations[C]?.items.map((i) => i.id)).toEqual(["proposal:p1", "proposal:p2"]);
    chat.syncPending(S, [view("p1"), view("p2")], true);
    expect(useChat.getState().conversations[C]?.items).toHaveLength(2);
    // session_changed (not authoritative) never expires anything.
    chat.syncPending(S, [view("p1")], false);
    expect(useChat.getState().proposals.p2?.status).toBe("pending");
    chat.syncPending(S, [view("p1")], true);
    expect(useChat.getState().proposals.p2?.status).toBe("expired");
    expect(useChat.getState().proposals.p1?.status).toBe("pending");
    // A decided proposal is not resurrected by a stale list.
    chat.resolveProposal("p1", "accept", "user");
    chat.syncPending(S, [view("p1")], true);
    expect(useChat.getState().proposals.p1?.status).toBe("accept");
  });
});
