// Session lifecycle, resync, saving and chat against a small fake of the Backend interface
// (IPC.md section 7). The fake enforces the boundary rules (open before subscribe, the index
// needs a ready kernel); each test scripts the events it needs. The same paths run against
// the real core in the Playwright suite (e2e/review-fixes.spec.ts, e2e/workbench.spec.ts).
import { beforeEach, describe, expect, it } from "vitest";
import { setBackend } from "../ipc";
import type { ProposalView, SessionInfo, TranscriptEvent } from "../ipc";
import { FakeBackend, NOW, ipcError, runView } from "../test/fake";
import { resetStores, typeIn, waitFor } from "../test/util";
import {
  bootstrap,
  closeSession,
  createSession,
  loadBuffer,
  openSession,
  openTerminal,
  respondProposal,
  runCurrentScript,
  saveBuffer,
  selectConversation,
  sendChat,
} from "./actions";
import { useChat } from "./chat";
import { isSubscribed, subscribeSession } from "./dispatcher";
import { bufferText, useFiles } from "./files";
import { flushRunOutput, useRuns } from "./runs";
import { useSessions } from "./sessions";
import { bufferedTerminalChars, useTerminals } from "./terminals";
import { useUi } from "./ui";

let b: FakeBackend;
let seeded: SessionInfo;

function storedTurn(info: SessionInfo, run_id: string): TranscriptEvent[] {
  const args = ["--tau", "2.0"];
  return [
    { ts: NOW, type: "user", text: "Run the decay simulation at tau = 2." },
    { ts: NOW, type: "assistant", text: "Running it. " },
    { ts: NOW, type: "tool_call", id: "t1", name: "run.script", arguments: { script: "scripts/decay_sim.py", args } },
    { ts: NOW, type: "run", run_id, script: "scripts/decay_sim.py", args, target: info.target },
    { ts: NOW, type: "tool_result", id: "t1", ok: true, result: { run_id, exit_code: 0 } },
    { ts: NOW, type: "assistant", text: "The fit agrees." },
    { ts: NOW, type: "summary", text: "Ran decay_sim.py once.", files_changed: [], runs: [run_id], figures: [] },
  ];
}

function connect(): FakeBackend {
  b = new FakeBackend();
  seeded = b.seed({ id: "s1", name: "decay-analysis" });
  const run = runView(seeded, "run-seed", { status: "done", exit_code: 0, finished_at: NOW }, seeded.active_conversation);
  b.session("s1").runs.push(run);
  b.session("s1").transcripts.set(seeded.active_conversation, storedTurn(seeded, "run-seed"));
  setBackend(b);
  return b;
}

const kernel = (s: string) => useSessions.getState().kernel[s]?.state;
const errors = () => useUi.getState().toasts.filter((t) => t.kind === "error");

beforeEach(() => {
  resetStores();
});

describe("opening an existing (closed) session", () => {
  it("a subscription before open is refused, and the failed attempt leaves nothing behind", async () => {
    connect();
    await bootstrap();
    expect(useSessions.getState().sessions.map((s) => [s.id, s.open])).toEqual([["s1", false]]);
    await expect(subscribeSession("s1")).rejects.toMatchObject({ code: "not_found" });
    expect(isSubscribed("s1")).toBe(false);
    // The retry is a real attempt, not a no-op against a stale map entry.
    await openSession("s1");
    expect(isSubscribed("s1")).toBe(true);
    b.kernelReady("s1");
    await waitFor(() => kernel("s1") === "ready", 4000, "kernel ready through the event channel");
  });

  it("calls session_open, then subscribe, then session_info, and does not wait for the kernel", async () => {
    connect();
    await bootstrap();
    b.calls.length = 0;
    await openSession("s1");
    const order = b.calls.filter((c) => c === "session_open" || c === "subscribe" || c === "session_info");
    expect(order.slice(0, 3)).toEqual(["session_open", "subscribe", "session_info"]);
    expect(kernel("s1")).toBe("starting");
    expect(useSessions.getState().active).toBe("s1");
    expect(useSessions.getState().sessions[0]?.open).toBe(true);
  });

  it("loads tree, runs and the transcript without the kernel; the index follows kernel_state: ready", async () => {
    connect();
    await bootstrap();
    await openSession("s1");
    await waitFor(() => useFiles.getState().trees.s1?.[""], 4000, "file tree");
    await waitFor(() => (useRuns.getState().runs.s1?.length ?? 0) === 1, 4000, "runs");
    await waitFor(() => useChat.getState().conversations[seeded.active_conversation]?.loaded, 4000, "transcript");
    expect(kernel("s1")).toBe("starting");
    expect(useFiles.getState().index.s1 ?? null).toBeNull();
    expect(useFiles.getState().indexStale.s1).toBe(true);
    expect(errors()).toEqual([]);
    b.kernelReady("s1");
    const index = await waitFor(() => useFiles.getState().index.s1, 4000, "index after kernel ready");
    expect(index.scripts.map((s) => s.name)).toContain("decay_sim");
  });

  it("maps the stored transcript into chat items and attaches the run card", async () => {
    connect();
    await bootstrap();
    await openSession("s1");
    const conv = await waitFor(() => {
      const c = useChat.getState().conversations[seeded.active_conversation];
      return c?.loaded ? c : undefined;
    });
    expect(conv.items.map((i) => i.kind)).toEqual(["user", "assistant", "tool", "run", "assistant", "summary"]);
    expect(conv.items[0]).toMatchObject({ kind: "user", text: expect.stringContaining("decay simulation") });
    expect(conv.items[2]).toMatchObject({ kind: "tool", name: "run.script", result: { ok: true } });
    expect(conv.busy).toBe(false);
  });

  it("concurrent opens share one session_open and one subscription", async () => {
    connect();
    await bootstrap();
    b.calls.length = 0;
    await Promise.all([openSession("s1"), openSession("s1"), openSession("s1")]);
    expect(b.calls.filter((c) => c === "session_open")).toHaveLength(1);
    expect(b.calls.filter((c) => c === "subscribe")).toHaveLength(1);
  });

  it("the index is refetched on index_changed", async () => {
    connect();
    await bootstrap();
    await openSession("s1");
    b.kernelReady("s1");
    await waitFor(() => useFiles.getState().index.s1);
    b.calls.length = 0;
    b.emit("s1", { type: "index_changed" });
    await waitFor(() => b.calls.includes("index_get"), 4000, "index_get after index_changed");
  });
});

describe("creating and closing", () => {
  it("createSession returns before the kernel is ready and reports no failure", async () => {
    connect();
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x", target: "local" });
    expect(kernel(id)).toBe("starting");
    await waitFor(() => isSubscribed(id));
    b.kernelReady(id);
    await waitFor(() => kernel(id) === "ready");
    await waitFor(() => useFiles.getState().index[id]);
    expect(errors()).toEqual([]);
  });

  it("closing clears the session's terminals and tolerates the redundant unsubscribe", async () => {
    connect();
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    await waitFor(() => isSubscribed(id));
    await openTerminal(id);
    const term = useTerminals.getState().terminals[id]?.[0]?.id;
    expect(term).toBeDefined();
    expect(await closeSession(id)).toBe(true);
    expect(useTerminals.getState().terminals[id]).toEqual([]);
    expect(bufferedTerminalChars(term ?? "")).toBe(0);
    expect(isSubscribed(id)).toBe(false);
    expect(useSessions.getState().sessions.find((s) => s.id === id)?.open).toBe(false);
    // And it can be opened again with a working event channel.
    await openSession(id);
    b.kernelReady(id);
    await waitFor(() => kernel(id) === "ready");
  });

  it("asks before closing a session with unsaved buffers; cancel keeps it open", async () => {
    connect();
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    await loadBuffer(id, "notes.md");
    typeIn(id, "notes.md", "changed\n");
    const closing = closeSession(id);
    const prompt = await waitFor(() => useUi.getState().unsavedPrompt);
    expect(prompt.message).toContain("notes.md");
    prompt.resolve("cancel");
    expect(await closing).toBe(false);
    expect(useSessions.getState().sessions.find((s) => s.id === id)?.open).toBe(true);
    const again = closeSession(id);
    (await waitFor(() => useUi.getState().unsavedPrompt)).resolve("save");
    expect(await again).toBe(true);
    expect(b.session(id).files.get("notes.md")).toBe("changed\n");
  });
});

describe("resync", () => {
  it("refetches session info, runs, figures and the active transcript, and reconciles run output", async () => {
    connect();
    await bootstrap();
    await openSession("s1");
    b.kernelReady("s1");
    await waitFor(() => kernel("s1") === "ready");
    const conv = seeded.active_conversation;
    await waitFor(() => useChat.getState().conversations[conv]?.loaded);

    // A run starts while we are listening, then the subscriber falls behind.
    const run = await b.invoke("run_start", { session: "s1", script: "scripts/decay_sim.py", args: ["--tau", "1.5"] });
    await waitFor(() => useRuns.getState().runs.s1?.some((r) => r.meta.run_id === run.meta.run_id));
    b.dropEvents = true;
    // Meanwhile: the mode changed, a plan turn ran, and the run finished.
    b.session("s1").info.conversations[0]!.mode = "plan";
    b.session("s1").transcripts.get(conv)!.push({ ts: NOW, type: "user", text: "Plan a comparison" }, { ts: NOW, type: "summary", text: "Planned.", files_changed: [], runs: [], figures: [] });
    b.finishRun("s1", run.meta.run_id, "tau_fit = 1.49\n", "fit 100%\n");
    flushRunOutput();
    expect(useRuns.getState().runs.s1?.find((r) => r.meta.run_id === run.meta.run_id)?.meta.status).toBe("running");
    expect(useChat.getState().conversations[conv]?.items.some((i) => i.kind === "user" && i.text === "Plan a comparison")).toBe(false);

    b.dropEvents = false;
    b.calls.length = 0;
    b.emit("s1", { type: "resync" });

    await waitFor(() => useRuns.getState().runs.s1?.find((r) => r.meta.run_id === run.meta.run_id)?.meta.status === "done", 4000, "run reconciled");
    const out = await waitFor(() => useRuns.getState().output[run.meta.run_id], 4000, "output from run_get");
    expect(out.stdout.text).toContain("tau_fit");
    await waitFor(() => useChat.getState().conversations[conv]?.items.some((i) => i.kind === "user" && i.text === "Plan a comparison"), 4000, "transcript refetched");
    expect(useChat.getState().conversations[conv]?.items.filter((i) => i.kind === "summary")).toHaveLength(2);
    expect(useSessions.getState().infos.s1?.conversations[0]?.mode).toBe("plan");
    for (const cmd of ["session_info", "runs_list", "figures_list", "conversation_transcript", "run_get"] as const) expect(b.calls).toContain(cmd);
  });

  it("run_done reconciles output through run_get; a carriage-return progress bar keeps its final state", async () => {
    connect();
    await bootstrap();
    await openSession("s1");
    b.kernelReady("s1");
    await waitFor(() => kernel("s1") === "ready");
    const run = await b.invoke("run_start", { session: "s1", script: "scripts/decay_sim.py", args: [] });
    b.calls.length = 0;
    b.finishRun("s1", run.meta.run_id, "tau_fit = 2.5\n", "fit  25%\rfit 100%\n");
    await waitFor(() => useRuns.getState().runs.s1?.find((r) => r.meta.run_id === run.meta.run_id)?.meta.status === "done");
    await waitFor(() => b.calls.includes("run_get"));
    const out = await waitFor(() => useRuns.getState().output[run.meta.run_id]?.stdout.text ? useRuns.getState().output[run.meta.run_id] : undefined);
    expect(out.stdout.text).toBe("tau_fit = 2.5\n");
    expect(out.stderr.text).toBe("fit 100%\n");
  });
});

describe("pending proposals and busy state after a reload", () => {
  function pendingEdit(info: SessionInfo): ProposalView {
    return {
      id: "p1",
      conversation: info.active_conversation,
      kind: "edit",
      path: "analysis/models.py",
      summary: "Add double_exp",
      original: "def single_exp(): ...\n",
      proposed: "def single_exp(): ...\n\ndef double_exp(): ...\n",
      diff: null,
      command: null,
      packages: null,
      message: null,
      in_scope: true,
      reason: "review",
    } as ProposalView;
  }

  it("shows live proposal cards from SessionInfo.pending_proposals and seeds busy", async () => {
    connect();
    const s = b.session("s1");
    s.info.open = true;
    s.info.conversations[0]!.busy = true;
    s.info.pending_proposals = [pendingEdit(s.info)];
    const conv = s.info.active_conversation;
    // The webview (re)loads: the backend still has the session open with a paused turn.
    await bootstrap();
    expect(useSessions.getState().active).toBe("s1");
    const c = await waitFor(() => {
      const x = useChat.getState().conversations[conv];
      return x?.loaded ? x : undefined;
    });
    expect(c.busy).toBe(true);
    expect(useChat.getState().proposals.p1).toMatchObject({ status: "pending", proposal: { path: "analysis/models.py" } });
    expect(useChat.getState().proposals.p1?.proposal.proposed).toContain("double_exp");
    expect(c.items.filter((i) => i.kind === "proposal")).toHaveLength(1);

    // The card works: accepting sends the decision, and the turn's end arrives as events.
    b.handle("proposal_respond", () => {
      s.info.pending_proposals = [];
      s.info.conversations[0]!.busy = false;
      queueMicrotask(() => {
        b.emit("s1", { type: "proposal_resolved", proposal_id: "p1", decision: "accept", by: "user" } as never);
        b.emit("s1", { type: "turn_done", conversation: conv, turn: "t1", summary: { text: "Added double_exp.", files_changed: ["analysis/models.py"], runs: [], figures: [] } });
      });
      return null;
    });
    expect(await respondProposal("s1", "p1", { decision: "accept" })).toBe(true);
    await waitFor(() => useChat.getState().conversations[conv]?.items.some((i) => i.kind === "summary" && i.summary.text === "Added double_exp."), 4000, "turn finished");
    expect(useChat.getState().conversations[conv]?.busy).toBe(false);
  });

  it("respondProposal guards against double submission", async () => {
    connect();
    const s = b.session("s1");
    s.info.open = true;
    s.info.conversations[0]!.busy = true;
    s.info.pending_proposals = [pendingEdit(s.info)];
    b.handle("proposal_respond", () => null);
    await bootstrap();
    await waitFor(() => useChat.getState().proposals.p1);
    b.calls.length = 0;
    const results = await Promise.all([respondProposal("s1", "p1", { decision: "reject" }), respondProposal("s1", "p1", { decision: "accept" }), respondProposal("s1", "p1", { decision: "accept" })]);
    expect(results).toEqual([true, false, false]);
    expect(b.calls.filter((c) => c === "proposal_respond")).toHaveLength(1);
    expect(useChat.getState().proposals.p1?.status).toBe("reject");
    expect(errors()).toEqual([]);
  });

  it("loads another conversation's transcript when it is selected", async () => {
    connect();
    await bootstrap();
    await openSession("s1");
    const conv = seeded.active_conversation;
    await waitFor(() => useChat.getState().conversations[conv]?.loaded);
    const other = await b.invoke("conversation_new", { session: "s1" });
    b.calls.length = 0;
    await selectConversation("s1", conv);
    await selectConversation("s1", other.id);
    expect(useChat.getState().conversations[other.id]?.loaded).toBe(true);
    // Already loaded conversations are not fetched again.
    expect(b.calls.filter((c) => c === "conversation_transcript")).toHaveLength(1);
  });
});

describe("chat send", () => {
  it("takes the optimistic message back and restores the draft when the send fails", async () => {
    connect();
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    const conv = useSessions.getState().infos[id]?.active_conversation ?? "";
    b.handle("chat_send", () => Promise.reject(ipcError("internal", "provider unreachable")));
    useChat.getState().setDraft(conv, "hello");
    useChat.getState().addAttachment(conv, { kind: "file", path: "notes.md" });
    await sendChat(id, conv, "hello", [{ kind: "file", path: "notes.md" }]);
    const c = useChat.getState();
    expect(c.conversations[conv]?.items).toEqual([]);
    expect(c.conversations[conv]?.busy).toBe(false);
    expect(c.drafts[conv]).toBe("hello");
    expect(c.pendingAttachments[conv]).toEqual([{ kind: "file", path: "notes.md" }]);
    expect(useUi.getState().toasts.some((t) => t.kind === "error" && t.text.includes("provider unreachable"))).toBe(true);
  });
});

describe("saving", () => {
  async function session(): Promise<string> {
    connect();
    await bootstrap();
    return createSession({ repo_abs: "/home/user/projects/x" });
  }

  it("marks saved with the content actually written; text typed during the save stays dirty", async () => {
    const id = await session();
    await loadBuffer(id, "notes.md");
    let release: () => void = () => undefined;
    const gate = new Promise<void>((r) => (release = r));
    b.intercept = async (cmd, _a, next) => {
      if (cmd === "fs_write") await gate;
      return next();
    };
    typeIn(id, "notes.md", "first\n");
    const saving = saveBuffer(id, "notes.md");
    await waitFor(() => b.calls.includes("fs_write"));
    typeIn(id, "notes.md", "first\nsecond\n");
    release();
    expect(await saving).toBe(true);
    const buf = useFiles.getState().buffers[id]?.["notes.md"];
    expect(buf?.saved).toBe("first\n");
    expect(bufferText(id, "notes.md")).toBe("first\nsecond\n");
    expect(buf?.dirty).toBe(true);
    expect(b.session(id).files.get("notes.md")).toBe("first\n");
    // The next save sends the hash the first one produced: no false conflict.
    expect(await saveBuffer(id, "notes.md")).toBe(true);
    expect(useFiles.getState().buffers[id]?.["notes.md"]?.dirty).toBe(false);
    expect(b.session(id).files.get("notes.md")).toBe("first\nsecond\n");
    expect(useFiles.getState().buffers[id]?.["notes.md"]?.external).toBe("none");
  });

  it("serialises saves of one path", async () => {
    const id = await session();
    await loadBuffer(id, "notes.md");
    let active = 0;
    let peak = 0;
    const written: string[] = [];
    b.intercept = async (cmd, args, next) => {
      if (cmd !== "fs_write") return next();
      active++;
      peak = Math.max(peak, active);
      await new Promise((r) => setTimeout(r, 15));
      written.push((args as { content: string }).content);
      try {
        return await next();
      } finally {
        active--;
      }
    };
    typeIn(id, "notes.md", "one\n");
    const a = saveBuffer(id, "notes.md");
    typeIn(id, "notes.md", "two\n");
    const c = saveBuffer(id, "notes.md");
    const d = saveBuffer(id, "notes.md");
    expect(await Promise.all([a, c, d])).toEqual([true, true, true]);
    expect(peak).toBe(1);
    // One write carried the latest text; the queued saves found nothing left to write.
    expect(written).toEqual(["two\n"]);
    expect(useFiles.getState().buffers[id]?.["notes.md"]).toMatchObject({ dirty: false, saved: "two\n" });
    expect(errors()).toEqual([]);
  });

  it("round-trips a CRLF file: it opens clean and is written back with CRLF", async () => {
    const id = await session();
    const crlf = "a = 1\r\nb = 2\r\n";
    b.session(id).files.set("win.py", crlf);
    await loadBuffer(id, "win.py");
    const buf = useFiles.getState().buffers[id]?.["win.py"];
    expect(buf).toMatchObject({ dirty: false, lineSeparator: "\r\n", content: crlf });
    // What the editor reports after an edit (see editorState.test.ts) keeps the separator.
    typeIn(id, "win.py", "a = 1\r\nb = 2\r\nc = 3\r\n");
    expect(await saveBuffer(id, "win.py")).toBe(true);
    expect(b.session(id).files.get("win.py")).toBe("a = 1\r\nb = 2\r\nc = 3\r\n");
  });

  it("run current script is aborted when the save fails", async () => {
    const id = await session();
    await waitFor(() => isSubscribed(id));
    b.kernelReady(id);
    await waitFor(() => kernel(id) === "ready");
    await loadBuffer(id, "scripts/decay_sim.py");
    typeIn(id, "scripts/decay_sim.py", "print('edited')\n");
    b.intercept = (cmd, _a, next) => (cmd === "fs_write" ? Promise.reject(ipcError("io", "disk full")) : next());
    b.calls.length = 0;
    expect(await runCurrentScript(id, "scripts/decay_sim.py")).toBeNull();
    expect(b.calls).not.toContain("run_start");
    expect(useFiles.getState().buffers[id]?.["scripts/decay_sim.py"]?.dirty).toBe(true);
    // With a working disk the script is saved first, then run.
    b.intercept = (_c, _a, next) => next();
    expect(await runCurrentScript(id, "scripts/decay_sim.py")).not.toBeNull();
    expect(b.calls.indexOf("fs_write")).toBeLessThan(b.calls.indexOf("run_start"));
  });
});
