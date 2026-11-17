// The "feel" fixes of DESIGN-REVIEW.md: chat text batched per frame, one index
// fetch per change, figures grouped by stem and loaded through the asset
// protocol, a cached package list, and the policy presets.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setBackend } from "../ipc";
import type { FigureView, PolicyView, SessionEvent } from "../ipc";
import { probe, resetStores, strictMock, waitFor } from "../test/util";
import type { Probe } from "../test/util";
import { bootstrap, openSession } from "./actions";
import { flushAgentText, queueAgentText, useChat } from "./chat";
import { applyEvent } from "./dispatcher";
import { figureAssetUrl, figureStem, groupFigures } from "./figures";
import { ensurePackages, refreshPackages, usePackages } from "./packages";
import { POLICY_PRESETS, presetOf, presetPatch } from "./policyPresets";
import { useRuns } from "./runs";
import { useFiles } from "./files";
import { useSessions } from "./sessions";

const S = "s1";
const C = "s1-c1";

describe("chat streaming", () => {
  beforeEach(() => {
    resetStores();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("batches agent_text: many deltas become one store update per frame", () => {
    let updates = 0;
    const unsubscribe = useChat.subscribe(() => updates++);
    for (let i = 0; i < 2000; i++) applyEvent(S, { type: "agent_text", conversation: C, turn: "t1", delta: `w${i} ` });
    expect(updates).toBe(0);
    expect(useChat.getState().conversations[C]).toBeUndefined();
    vi.advanceTimersByTime(40);
    flushAgentText();
    expect(updates).toBe(1);
    const conv = useChat.getState().conversations[C]!;
    expect(conv.items).toHaveLength(1);
    expect(conv.stream?.text.startsWith("w0 w1 w2 ")).toBe(true);
    expect(conv.stream?.text.endsWith("w1999 ")).toBe(true);
    unsubscribe();
  });

  it("a delta does not copy the item list or touch other items", () => {
    const chat = useChat.getState();
    chat.addUser(C, "question", []);
    chat.toolCall(C, "t1", "call1", "fs.read", {});
    queueAgentText(C, "t1", "first ");
    flushAgentText();
    const before = useChat.getState().conversations[C]!;
    expect(before.items.map((i) => i.kind)).toEqual(["user", "tool", "assistant"]);
    for (let i = 0; i < 50; i++) {
      queueAgentText(C, "t1", "more ");
      flushAgentText();
    }
    const after = useChat.getState().conversations[C]!;
    // Same array, same item objects: memoised rows have nothing to re-render.
    expect(after.items).toBe(before.items);
    expect(after.stream?.text).toBe(`first ${"more ".repeat(50)}`);
    expect(after.stream?.id).toBe(after.items[2]!.id);
  });

  it("keeps order: queued text lands before the event that follows it, and the text is committed to its item", () => {
    const events: SessionEvent[] = [
      { type: "turn_started", conversation: C, turn: "t1" },
      { type: "agent_text", conversation: C, turn: "t1", delta: "Reading " },
      { type: "agent_text", conversation: C, turn: "t1", delta: "the file." },
      { type: "agent_tool_call", conversation: C, turn: "t1", id: "c1", name: "fs.read", arguments: {} },
      { type: "agent_tool_result", conversation: C, turn: "t1", id: "c1", ok: true, summary: "ok" },
      { type: "agent_text", conversation: C, turn: "t1", delta: "Done." },
      { type: "turn_done", conversation: C, turn: "t1", summary: { text: "done", files_changed: [], runs: [], figures: [] } },
    ];
    for (const e of events) applyEvent(S, e);
    const conv = useChat.getState().conversations[C]!;
    expect(conv.items.map((i) => i.kind)).toEqual(["assistant", "tool", "assistant", "summary"]);
    expect(conv.items[0]).toMatchObject({ text: "Reading the file.", streaming: false });
    expect(conv.items[2]).toMatchObject({ text: "Done.", streaming: false });
    expect(conv.stream).toBeNull();
  });

  it("text of two turns queued in one frame stays in two messages", () => {
    queueAgentText(C, "t1", "one");
    queueAgentText(C, "t2", "two");
    flushAgentText();
    const conv = useChat.getState().conversations[C]!;
    expect(conv.items).toHaveLength(2);
    expect(conv.items[0]).toMatchObject({ kind: "assistant", turn: "t1", text: "one", streaming: false });
    expect(conv.stream).toMatchObject({ turn: "t2", text: "two" });
  });
});

describe("index fetches", () => {
  let b: Probe;
  const count = () => b.calls.filter((c) => c === "index_get").length;

  beforeEach(async () => {
    resetStores();
    b = probe(strictMock(20));
    setBackend(b);
    await bootstrap();
    await openSession(S);
    await waitFor(() => useFiles.getState().index[S], 4000, "first index");
    // Let the load-time and kernel-ready requests settle.
    await new Promise((r) => setTimeout(r, 30));
    b.calls.length = 0;
  });

  it("files_changed of a .py file does not fetch the index; index_changed fetches it once", async () => {
    applyEvent(S, { type: "files_changed", paths: ["scripts/decay_sim.py", "analysis/models.py"] });
    await new Promise((r) => setTimeout(r, 30));
    expect(count()).toBe(0);
    expect(useFiles.getState().indexStale[S]).toBe(true);
    applyEvent(S, { type: "index_changed" });
    await waitFor(() => useFiles.getState().indexStale[S] === false, 4000, "index refreshed");
    expect(count()).toBe(1);
  });

  it("a real save (files_changed + index_changed) costs one index_get", async () => {
    const src = await b.invoke("fs_read", { session: S, path: "scripts/decay_sim.py" });
    await b.invoke("fs_write", { session: S, path: "scripts/decay_sim.py", content: `${src.content}\n# edited\n` });
    await waitFor(() => count() > 0 && useFiles.getState().indexStale[S] === false, 4000, "index refreshed");
    await new Promise((r) => setTimeout(r, 30));
    expect(count()).toBe(1);
  });

  it("coalesces a burst: requests during a fetch share it and cause one follow-up", async () => {
    let release: () => void = () => undefined;
    const gate = new Promise<void>((r) => (release = r));
    b.intercept = async (cmd, _a, next) => {
      if (cmd === "index_get") await gate;
      return next();
    };
    for (let i = 0; i < 10; i++) applyEvent(S, { type: "index_changed" });
    applyEvent(S, { type: "kernel_state", state: "ready", message: null });
    expect(count()).toBe(1);
    release();
    await waitFor(() => useFiles.getState().indexStale[S] === false, 4000, "index refreshed");
    expect(count()).toBe(2);
  });
});

function fig(path: string, format: FigureView["format"], file_abs: string | null = null): FigureView {
  const base = path.slice(path.lastIndexOf("/") + 1);
  return { run_id: "r1", script: "scripts/fit.py", path, name: base.replace(/\.[^.]+$/, ""), format, sidecar: null, created_at: "", size: 1, file_abs };
}

describe("figures", () => {
  beforeEach(() => resetStores());
  afterEach(() => {
    delete window.__TAURI_INTERNALS__;
  });

  it("groups files by stem: one entry per figure, PNG first, other formats as alternates", () => {
    const entries = groupFigures([
      fig("out/fit/r1/figures/residuals.svg", "svg"),
      fig("out/fit/r1/figures/residuals.png", "png"),
      fig("out/fit/r1/figures/decay.v2.png", "png"),
      fig("out/fit/r1/figures/residuals.pdf", "pdf"),
      fig("out/fit/r1/figures/vector-only.pdf", "pdf"),
      fig("out/fit/r1/figures/vector-only.svg", "svg"),
    ]);
    expect(entries.map((e) => e.name)).toEqual(["residuals", "decay.v2", "vector-only"]);
    expect(entries[0]!.primary.format).toBe("png");
    expect(entries[0]!.formats.map((f) => f.format)).toEqual(["png", "svg", "pdf"]);
    expect(entries[1]!.formats).toHaveLength(1);
    // Without a PNG the next best format is shown.
    expect(entries[2]!.primary.format).toBe("svg");
    expect(figureStem({ path: "figures/a.b.png", name: "x" })).toBe("a.b");
  });

  it("a figure arriving in two formats is one selection, not two", () => {
    const run = { meta: { run_id: "r1", script: "s", args: [], status: "running" as const, exit_code: null, started_at: "", finished_at: null, out_dir: "out", sandbox: "none", tree_hash: null, pid: null, host: "h" }, target: "local", artifacts: [], stdout_tail: "", stderr_tail: "", conversation: null };
    applyEvent(S, { type: "run_started", run });
    for (const f of [fig("out/figures/residuals.png", "png"), fig("out/figures/residuals.svg", "svg")]) {
      applyEvent(S, { type: "run_artifact", run_id: "r1", artifact: { kind: "figure", path: f.path, size: 1, created_at: "", rendered: true }, figure: f });
    }
    const st = useRuns.getState();
    expect(st.activeFigure[S]).toEqual({ name: "residuals", run_id: "r1" });
    expect(groupFigures(st.figures[S]![0]!.figures)).toHaveLength(1);
  });

  it("uses the asset protocol only under Tauri and only with a local file", () => {
    const local = fig("out/figures/a.png", "png", "/home/u/repo/out/figures/a.png");
    // Browser mode: no asset protocol, the bytes come through figure_data.
    expect(figureAssetUrl(local)).toBeNull();
    const convert = vi.fn((path: string, protocol: string) => `${protocol}://localhost/${encodeURIComponent(path)}`);
    window.__TAURI_INTERNALS__ = { convertFileSrc: convert };
    expect(figureAssetUrl(local)).toBe("asset://localhost/%2Fhome%2Fu%2Frepo%2Fout%2Ffigures%2Fa.png");
    expect(convert).toHaveBeenCalledTimes(1);
    // Not pulled from the remote target yet.
    expect(figureAssetUrl(fig("out/figures/b.png", "png", null))).toBeNull();
  });
});

describe("package list cache", () => {
  let b: Probe;
  const count = () => b.calls.filter((c) => c === "env_packages").length;

  beforeEach(async () => {
    resetStores();
    b = probe(strictMock(10));
    setBackend(b);
    await bootstrap();
    await openSession(S);
    await waitFor(() => useSessions.getState().kernel[S]?.state === "ready", 4000, "kernel");
    b.calls.length = 0;
  });

  it("fetches once, serves the cache afterwards, and refetches only after packages_changed or a refresh", async () => {
    await Promise.all([ensurePackages(S), ensurePackages(S)]);
    expect(count()).toBe(1);
    expect(usePackages.getState().lists[S]?.length).toBeGreaterThan(0);
    // Opening the panel again.
    await ensurePackages(S);
    await ensurePackages(S);
    expect(count()).toBe(1);
    // The event only marks the list stale; the next display fetches.
    applyEvent(S, { type: "packages_changed" });
    expect(usePackages.getState().stale[S]).toBe(true);
    expect(count()).toBe(1);
    await ensurePackages(S);
    expect(count()).toBe(2);
    expect(usePackages.getState().stale[S]).toBe(false);
    await ensurePackages(S);
    expect(count()).toBe(2);
    await refreshPackages(S);
    expect(count()).toBe(3);
  });

  it("a packages_changed during a fetch is not lost", async () => {
    let release: () => void = () => undefined;
    const gate = new Promise<void>((r) => (release = r));
    b.intercept = async (cmd, _a, next) => {
      if (cmd === "env_packages") await gate;
      return next();
    };
    const loading = ensurePackages(S);
    await waitFor(() => count() === 1);
    applyEvent(S, { type: "packages_changed" });
    release();
    await loading;
    expect(count()).toBe(2);
    expect(usePackages.getState().stale[S]).toBe(false);
  });
});

describe("policy presets", () => {
  const base: PolicyView = { edits: "review", runs: "auto", packages: "ask", shell: "ask", network: "deny", git: "ask", protected: [".env"], max_steps: 40 };

  it("map onto the session_set_policy fields of IPC.md section 8", () => {
    expect(presetPatch("review")).toEqual({ edits: "review", runs: "auto", packages: "ask", shell: "ask", git: "ask" });
    expect(presetPatch("auto")).toEqual({ edits: "auto", runs: "auto", packages: "ask", shell: "ask", git: "ask" });
    // Edits have no "deny" in session_set_policy; "review" is the strictest value there is.
    expect(presetPatch("read-only")).toEqual({ edits: "review", runs: "auto", packages: "deny", shell: "deny", git: "deny" });
    expect(POLICY_PRESETS.map((p) => p.id)).toEqual(["review", "auto", "read-only"]);
  });

  it("a preset never touches network, protected globs or the step limit", () => {
    for (const p of POLICY_PRESETS) expect(Object.keys(presetPatch(p.id)).sort()).toEqual(["edits", "git", "packages", "runs", "shell"]);
  });

  it("recognises the preset a policy amounts to, and custom combinations as none", () => {
    expect(presetOf(base)).toBe("review");
    for (const p of POLICY_PRESETS) expect(presetOf({ ...base, ...presetPatch(p.id) })).toBe(p.id);
    const otherKeys: PolicyView = { ...base, network: "allow", max_steps: 5 };
    expect(presetOf(otherKeys)).toBe("review");
    expect(presetOf({ ...base, edits: "auto-new-files" })).toBeNull();
    expect(presetOf({ ...base, runs: "ask" })).toBeNull();
  });

  it("applies through session_set_policy and leaves the other keys alone", async () => {
    resetStores();
    const b = probe(strictMock(10));
    setBackend(b);
    await bootstrap();
    await openSession(S);
    const before = useSessions.getState().infos[S]!.policy;
    const after = await b.invoke("session_set_policy", { session: S, policy: presetPatch("read-only") });
    expect(presetOf(after)).toBe("read-only");
    expect(after).toMatchObject({ network: before.network, protected: before.protected, max_steps: before.max_steps });
    await waitFor(() => presetOf(useSessions.getState().infos[S]!.policy) === "read-only", 4000, "session_changed");
  });
});
