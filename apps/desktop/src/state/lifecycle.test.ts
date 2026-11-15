// Session lifecycle, resync, saving and chat against the strict mock backend
// (IPC.md section 7). These are the paths that used to pass only because the
// mock was more forgiving than the real service.
import { beforeEach, describe, expect, it } from "vitest";
import { setBackend } from "../ipc";
import type { IpcError } from "../ipc";
import type { MockBackend } from "../ipc/mock";
import { probe, resetStores, strictMock, waitFor } from "../test/util";
import type { Probe } from "../test/util";
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
import { useFiles } from "./files";
import { flushRunOutput, useRuns } from "./runs";
import { useSessions } from "./sessions";
import { bufferedTerminalChars, useTerminals } from "./terminals";
import { useUi } from "./ui";

const SEEDED = "s1";
const CONV = "s1-c1";

let b: Probe;
let mock: MockBackend;

function connect(kernelMs = 60, seed = true): Probe {
  mock = strictMock(kernelMs, seed);
  b = probe(mock);
  setBackend(b);
  return b;
}

const kernel = (s: string) => useSessions.getState().kernel[s]?.state;

beforeEach(() => {
  resetStores();
});

describe("opening an existing (closed) session", () => {
  it("the mock rejects a subscription before open, and the failed attempt leaves nothing behind", async () => {
    connect();
    await bootstrap();
    expect(useSessions.getState().sessions.map((s) => [s.id, s.open])).toEqual([[SEEDED, false]]);
    await expect(subscribeSession(SEEDED)).rejects.toMatchObject({ code: "not_found" });
    expect(isSubscribed(SEEDED)).toBe(false);
    // The retry is a real attempt, not a no-op against a stale map entry.
    await openSession(SEEDED);
    expect(isSubscribed(SEEDED)).toBe(true);
    await waitFor(() => kernel(SEEDED) === "ready", 4000, "kernel ready through the event channel");
  });

  it("calls session_open, then subscribe, then session_info, and does not wait for the kernel", async () => {
    connect(150);
    await bootstrap();
    b.calls.length = 0;
    await openSession(SEEDED);
    const order = b.calls.filter((c) => c === "session_open" || c === "subscribe" || c === "session_info");
    expect(order.slice(0, 3)).toEqual(["session_open", "subscribe", "session_info"]);
    // Resolved while the kernel is still starting.
    expect(kernel(SEEDED)).toBe("starting");
    expect(useSessions.getState().active).toBe(SEEDED);
    expect(useSessions.getState().sessions[0]?.open).toBe(true);
  });

  it("loads tree, runs, figures and the transcript without the kernel; the index follows kernel_state: ready", async () => {
    connect(200);
    await bootstrap();
    await openSession(SEEDED);
    await waitFor(() => useFiles.getState().trees[SEEDED]?.[""], 4000, "file tree");
    await waitFor(() => (useRuns.getState().runs[SEEDED]?.length ?? 0) === 1, 4000, "runs");
    await waitFor(() => (useRuns.getState().figures[SEEDED]?.length ?? 0) === 1, 4000, "figures");
    await waitFor(() => useChat.getState().conversations[CONV]?.loaded, 4000, "transcript");
    // All of that arrived while index_get was still refused.
    expect(kernel(SEEDED)).toBe("starting");
    expect(useFiles.getState().index[SEEDED] ?? null).toBeNull();
    expect(useFiles.getState().indexStale[SEEDED]).toBe(true);
    expect(useUi.getState().toasts.filter((t) => t.kind === "error")).toEqual([]);
    const index = await waitFor(() => useFiles.getState().index[SEEDED], 4000, "index after kernel ready");
    expect(index.scripts.map((s) => s.name)).toContain("decay_sim");
    expect(kernel(SEEDED)).toBe("ready");
  });

  it("maps the stored transcript into chat items and attaches the run card", async () => {
    connect();
    await bootstrap();
    await openSession(SEEDED);
    const conv = await waitFor(() => {
      const c = useChat.getState().conversations[CONV];
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
    await Promise.all([openSession(SEEDED), openSession(SEEDED), openSession(SEEDED)]);
    expect(b.calls.filter((c) => c === "session_open")).toHaveLength(1);
    expect(b.calls.filter((c) => c === "subscribe")).toHaveLength(1);
  });

  it("the index is refetched on index_changed", async () => {
    connect(20);
    await bootstrap();
    await openSession(SEEDED);
    await waitFor(() => useFiles.getState().index[SEEDED]);
    const src = await b.invoke("fs_read", { session: SEEDED, path: "scripts/decay_sim.py" });
    await b.invoke("fs_write", { session: SEEDED, path: "scripts/decay_sim.py", content: src.content.replace("default=2.5", "default=4.0") });
    await waitFor(() => useFiles.getState().index[SEEDED]?.scripts.find((s) => s.name === "decay_sim")?.options.find((o) => o.dest === "tau")?.default === 4, 4000, "new default in the index");
  });
});

describe("creating and closing", () => {
  it("createSession returns before the kernel is ready and reports no failure", async () => {
    connect(200, false);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x", target: "local" });
    expect(kernel(id)).toBe("starting");
    expect(isSubscribed(id)).toBe(true);
    await waitFor(() => kernel(id) === "ready");
    await waitFor(() => useFiles.getState().index[id]);
    expect(useUi.getState().toasts.filter((t) => t.kind === "error")).toEqual([]);
  });

  it("closing clears the session's terminals and tolerates the redundant unsubscribe", async () => {
    connect(20, false);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
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
    await waitFor(() => kernel(id) === "ready");
  });

  it("asks before closing a session with unsaved buffers; cancel keeps it open", async () => {
    connect(20, false);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    await loadBuffer(id, "notes.md");
    useFiles.getState().updateContent(id, "notes.md", "changed\n");
    const closing = closeSession(id);
    const prompt = await waitFor(() => useUi.getState().unsavedPrompt);
    expect(prompt.message).toContain("notes.md");
    prompt.resolve("cancel");
    expect(await closing).toBe(false);
    expect(useSessions.getState().sessions.find((s) => s.id === id)?.open).toBe(true);
    const again = closeSession(id);
    (await waitFor(() => useUi.getState().unsavedPrompt)).resolve("save");
    expect(await again).toBe(true);
    await openSession(id);
    expect((await b.invoke("fs_read", { session: id, path: "notes.md" })).content).toBe("changed\n");
  });
});

describe("resync", () => {
  it("refetches session info, runs, figures and the active transcript, and reconciles run output", async () => {
    connect(20);
    await bootstrap();
    await openSession(SEEDED);
    await waitFor(() => kernel(SEEDED) === "ready");
    await waitFor(() => useChat.getState().conversations[CONV]?.loaded);

    // A run starts while we are listening, then the subscriber falls behind.
    const run = await b.invoke("run_start", { session: SEEDED, script: "scripts/decay_sim.py", args: ["--tau", "1.5"] });
    await waitFor(() => useRuns.getState().runs[SEEDED]?.some((r) => r.meta.run_id === run.meta.run_id));
    b.dropEvents = true;
    await b.invoke("chat_set_mode", { session: SEEDED, conversation: CONV, mode: "plan" });
    await b.invoke("chat_send", { session: SEEDED, conversation: CONV, text: "Plan a comparison" });
    // Wait (by polling the backend, since events are being dropped) for the turn and the run to finish.
    let transcript = await b.invoke("conversation_transcript", { session: SEEDED, conversation: CONV });
    for (let i = 0; i < 200 && transcript.filter((t) => t.type === "summary").length < 2; i++) {
      await new Promise((r) => setTimeout(r, 5));
      transcript = await b.invoke("conversation_transcript", { session: SEEDED, conversation: CONV });
    }
    for (let i = 0; i < 200; i++) {
      if ((await b.invoke("run_get", { session: SEEDED, run_id: run.meta.run_id })).meta.status === "done") break;
      await new Promise((r) => setTimeout(r, 5));
    }
    // Nothing of that reached the stores.
    flushRunOutput();
    expect(useRuns.getState().runs[SEEDED]?.find((r) => r.meta.run_id === run.meta.run_id)?.meta.status).toBe("running");
    expect(useChat.getState().conversations[CONV]?.items.some((i) => i.kind === "user" && i.text === "Plan a comparison")).toBe(false);
    expect(useSessions.getState().infos[SEEDED]?.conversations[0]?.mode).toBe("run");

    b.dropEvents = false;
    b.calls.length = 0;
    mock.debugEmit(SEEDED, { type: "resync" });

    await waitFor(() => useRuns.getState().runs[SEEDED]?.find((r) => r.meta.run_id === run.meta.run_id)?.meta.status === "done", 4000, "run reconciled");
    const out = await waitFor(() => useRuns.getState().output[run.meta.run_id], 4000, "output from run_get");
    expect(out.stdout.text).toContain("tau_fit");
    expect(useRuns.getState().runs[SEEDED]?.find((r) => r.meta.run_id === run.meta.run_id)?.artifacts.length).toBeGreaterThan(0);
    await waitFor(() => useRuns.getState().figures[SEEDED]?.some((g) => g.run.meta.run_id === run.meta.run_id), 4000, "figures refetched");
    await waitFor(() => useChat.getState().conversations[CONV]?.items.some((i) => i.kind === "user" && i.text === "Plan a comparison"), 4000, "transcript refetched");
    expect(useChat.getState().conversations[CONV]?.items.filter((i) => i.kind === "summary")).toHaveLength(2);
    expect(useChat.getState().conversations[CONV]?.busy).toBe(false);
    expect(useSessions.getState().infos[SEEDED]?.conversations[0]?.mode).toBe("plan");
    for (const cmd of ["session_info", "runs_list", "figures_list", "conversation_transcript", "run_get"] as const) expect(b.calls).toContain(cmd);
  });

  it("run_done reconciles output and artifacts through run_get", async () => {
    connect(20);
    await bootstrap();
    await openSession(SEEDED);
    await waitFor(() => kernel(SEEDED) === "ready");
    b.calls.length = 0;
    const run = await b.invoke("run_start", { session: SEEDED, script: "scripts/decay_sim.py", args: [] });
    await waitFor(() => useRuns.getState().runs[SEEDED]?.find((r) => r.meta.run_id === run.meta.run_id)?.meta.status === "done");
    await waitFor(() => b.calls.includes("run_get"));
    const out = await waitFor(() => useRuns.getState().output[run.meta.run_id]);
    const full = await b.invoke("run_get", { session: SEEDED, run_id: run.meta.run_id });
    expect(out.stdout.text).toBe(full.stdout);
    // The carriage-return progress bar collapsed to its final state.
    expect(out.stderr.text).toBe("fit 100%\n");
  });
});

describe("pending proposals and busy state after a reload", () => {
  it("shows live proposal cards from SessionInfo.pending_proposals and seeds busy", async () => {
    connect(20, false);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    await waitFor(() => kernel(id) === "ready");
    const conv = useSessions.getState().infos[id]?.active_conversation ?? "";
    await sendChat(id, conv, "Add a model", []);
    const pid = await waitFor(() => Object.keys(useChat.getState().proposals)[0], 4000, "proposal");

    // The webview reloads: every store is gone, the backend keeps the turn paused.
    resetStores();
    await bootstrap();
    expect(useSessions.getState().active).toBe(id);
    const c = await waitFor(() => {
      const x = useChat.getState().conversations[conv];
      return x?.loaded ? x : undefined;
    });
    expect(c.busy).toBe(true);
    expect(useChat.getState().proposals[pid]).toMatchObject({ status: "pending", proposal: { path: "analysis/models.py" } });
    expect(useChat.getState().proposals[pid]?.proposal.proposed).toContain("double_exp");
    expect(c.items.filter((i) => i.kind === "proposal")).toHaveLength(1);

    // The card works: accepting resumes the turn.
    expect(await respondProposal(id, pid, { decision: "accept" })).toBe(true);
    await waitFor(() => useChat.getState().conversations[conv]?.items.some((i) => i.kind === "summary"), 6000, "turn finished");
    expect(useChat.getState().conversations[conv]?.busy).toBe(false);
    // The run the turn started is attached by RunView.conversation.
    expect(useChat.getState().conversations[conv]?.items.filter((i) => i.kind === "run")).toHaveLength(1);
  });

  it("respondProposal guards against double submission", async () => {
    connect(20, false);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    await waitFor(() => kernel(id) === "ready");
    const conv = useSessions.getState().infos[id]?.active_conversation ?? "";
    await sendChat(id, conv, "Add a model", []);
    const pid = await waitFor(() => Object.keys(useChat.getState().proposals)[0]);
    b.calls.length = 0;
    const results = await Promise.all([respondProposal(id, pid, { decision: "reject" }), respondProposal(id, pid, { decision: "accept" }), respondProposal(id, pid, { decision: "accept" })]);
    expect(results).toEqual([true, false, false]);
    expect(b.calls.filter((c) => c === "proposal_respond")).toHaveLength(1);
    expect(useChat.getState().proposals[pid]?.status).toBe("reject");
    expect(useUi.getState().toasts.filter((t) => t.kind === "error")).toEqual([]);
  });

  it("loads another conversation's transcript when it is selected", async () => {
    connect(20);
    await bootstrap();
    await openSession(SEEDED);
    await waitFor(() => useChat.getState().conversations[CONV]?.loaded);
    const other = await b.invoke("conversation_new", { session: SEEDED });
    b.calls.length = 0;
    await selectConversation(SEEDED, CONV);
    await selectConversation(SEEDED, other.id);
    expect(useChat.getState().conversations[other.id]?.loaded).toBe(true);
    // Already loaded conversations are not fetched again.
    expect(b.calls.filter((c) => c === "conversation_transcript")).toHaveLength(1);
  });
});

describe("chat send", () => {
  it("takes the optimistic message back and restores the draft when the send fails", async () => {
    connect(20, false);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    const conv = useSessions.getState().infos[id]?.active_conversation ?? "";
    b.intercept = (cmd, _args, next) => (cmd === "chat_send" ? Promise.reject<unknown>({ code: "internal", message: "provider unreachable" } satisfies IpcError) : next());
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
    connect(20, false);
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
    useFiles.getState().updateContent(id, "notes.md", "first\n");
    const saving = saveBuffer(id, "notes.md");
    await waitFor(() => b.calls.includes("fs_write"));
    useFiles.getState().updateContent(id, "notes.md", "first\nsecond\n");
    release();
    expect(await saving).toBe(true);
    const buf = useFiles.getState().buffers[id]?.["notes.md"];
    expect(buf?.saved).toBe("first\n");
    expect(buf?.content).toBe("first\nsecond\n");
    expect(buf?.dirty).toBe(true);
    expect((await b.invoke("fs_read", { session: id, path: "notes.md" })).content).toBe("first\n");
    // The next save sends the hash the first one produced: no false conflict.
    expect(await saveBuffer(id, "notes.md")).toBe(true);
    expect(useFiles.getState().buffers[id]?.["notes.md"]?.dirty).toBe(false);
    expect((await b.invoke("fs_read", { session: id, path: "notes.md" })).content).toBe("first\nsecond\n");
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
    useFiles.getState().updateContent(id, "notes.md", "one\n");
    const a = saveBuffer(id, "notes.md");
    useFiles.getState().updateContent(id, "notes.md", "two\n");
    const c = saveBuffer(id, "notes.md");
    const d = saveBuffer(id, "notes.md");
    expect(await Promise.all([a, c, d])).toEqual([true, true, true]);
    expect(peak).toBe(1);
    // One write carried the latest text; the queued saves found nothing left to write.
    expect(written).toEqual(["two\n"]);
    expect(useFiles.getState().buffers[id]?.["notes.md"]).toMatchObject({ dirty: false, saved: "two\n" });
    expect(useUi.getState().toasts.filter((t) => t.kind === "error")).toEqual([]);
  });

  it("round-trips a CRLF file: it opens clean and is written back with CRLF", async () => {
    const id = await session();
    const crlf = "a = 1\r\nb = 2\r\n";
    await b.invoke("fs_write", { session: id, path: "win.py", content: crlf });
    await loadBuffer(id, "win.py");
    const buf = useFiles.getState().buffers[id]?.["win.py"];
    expect(buf).toMatchObject({ dirty: false, lineSeparator: "\r\n", content: crlf });
    // What the editor reports after an edit (see editorState.test.ts) keeps the separator.
    useFiles.getState().updateContent(id, "win.py", "a = 1\r\nb = 2\r\nc = 3\r\n");
    expect(await saveBuffer(id, "win.py")).toBe(true);
    expect((await b.invoke("fs_read", { session: id, path: "win.py" })).content).toBe("a = 1\r\nb = 2\r\nc = 3\r\n");
  });

  it("run current script is aborted when the save fails", async () => {
    const id = await session();
    await waitFor(() => kernel(id) === "ready");
    await loadBuffer(id, "scripts/decay_sim.py");
    useFiles.getState().updateContent(id, "scripts/decay_sim.py", "print('edited')\n");
    b.intercept = (cmd, _a, next) => (cmd === "fs_write" ? Promise.reject<unknown>({ code: "io", message: "disk full" } satisfies IpcError) : next());
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
