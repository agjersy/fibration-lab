// The mock must refuse what the real service refuses (IPC.md section 7), or
// renderer tests pass for the wrong reason.
import { describe, expect, it } from "vitest";
import { MockBackend } from "./mock";
import type { SessionEvent } from "./types";

const SEEDED = "s1";

function mock(timeScale = 0.2): MockBackend {
  return new MockBackend({ latency: [0, 0], timeScale });
}

function until(check: () => boolean, ms = 3000): Promise<void> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const tick = () => (check() ? resolve() : Date.now() - start > ms ? reject(new Error("timeout")) : void setTimeout(tick, 2));
    tick();
  });
}

describe("MockBackend lifecycle rules", () => {
  it("rejects subscribe and data commands on a session that is not open", async () => {
    const b = mock();
    expect((await b.invoke("sessions_list", {}))[0]).toMatchObject({ id: SEEDED, open: false });
    await expect(b.subscribe(SEEDED, () => undefined)).rejects.toMatchObject({ code: "not_found" });
    await expect(b.invoke("fs_tree", { session: SEEDED })).rejects.toMatchObject({ code: "not_found" });
    await expect(b.invoke("runs_list", { session: SEEDED })).rejects.toMatchObject({ code: "not_found" });
    await expect(b.invoke("conversation_transcript", { session: SEEDED, conversation: "s1-c1" })).rejects.toMatchObject({ code: "not_found" });
    await expect(b.subscribe("nope", () => undefined)).rejects.toMatchObject({ code: "not_found" });
  });

  it("session_open returns before the kernel is up; index_get is kernel_offline until kernel_state: ready", async () => {
    const b = mock();
    const info = await b.invoke("session_open", { session: SEEDED });
    expect(info).toMatchObject({ open: true, kernel: "starting", pending_proposals: [] });
    const events: SessionEvent[] = [];
    await b.subscribe(SEEDED, (e) => events.push(e));
    await expect(b.invoke("index_get", { session: SEEDED })).rejects.toMatchObject({ code: "kernel_offline" });
    await expect(b.invoke("run_start", { session: SEEDED, script: "scripts/decay_sim.py", args: [] })).rejects.toMatchObject({ code: "kernel_offline" });
    // Kernel-free data is available meanwhile.
    expect((await b.invoke("fs_tree", { session: SEEDED })).length).toBeGreaterThan(0);
    expect(await b.invoke("runs_list", { session: SEEDED })).toHaveLength(1);
    expect((await b.invoke("conversation_transcript", { session: SEEDED, conversation: "s1-c1" })).length).toBeGreaterThan(3);
    await until(() => events.some((e) => e.type === "kernel_state" && e.state === "ready"));
    expect((await b.invoke("index_get", { session: SEEDED })).scripts.length).toBeGreaterThan(0);
    expect((await b.invoke("session_info", { session: SEEDED })).kernel).toBe("ready");
  });

  it("closing a session ends its subscriptions and reopening needs a new one", async () => {
    const b = mock(0);
    await b.invoke("session_open", { session: SEEDED });
    const events: SessionEvent[] = [];
    const unsubscribe = await b.subscribe(SEEDED, (e) => events.push(e));
    await b.invoke("session_close", { session: SEEDED });
    expect(events.at(-1)).toMatchObject({ type: "kernel_state", state: "stopped" });
    // The redundant unsubscribe is harmless.
    expect(() => unsubscribe()).not.toThrow();
    events.length = 0;
    await b.invoke("session_open", { session: SEEDED });
    await new Promise((r) => setTimeout(r, 20));
    expect(events).toEqual([]);
  });

  it("reports pending proposals in SessionInfo, tags agent runs with their conversation, and cancels the turn's run", async () => {
    const b = mock(0);
    const info = await b.invoke("session_create", { repo_abs: "/home/user/projects/x" });
    const events: SessionEvent[] = [];
    await b.subscribe(info.id, (e) => events.push(e));
    await until(() => events.some((e) => e.type === "kernel_state" && e.state === "ready"));
    const conv = info.active_conversation;
    await b.invoke("chat_send", { session: info.id, conversation: conv, text: "Add a model" });
    await until(() => events.some((e) => e.type === "proposal"));
    const pending = (await b.invoke("session_info", { session: info.id })).pending_proposals;
    expect(pending).toHaveLength(1);
    expect(pending[0]).toMatchObject({ conversation: conv, kind: "edit" });
    await b.invoke("proposal_respond", { session: info.id, proposal_id: pending[0]?.id ?? "", decision: { decision: "accept" } });
    await until(() => events.some((e) => e.type === "run_started"));
    const started = events.find((e) => e.type === "run_started");
    expect(started?.type === "run_started" && started.run.conversation).toBe(conv);
    expect((await b.invoke("session_info", { session: info.id })).pending_proposals).toEqual([]);
    await b.invoke("chat_cancel", { session: info.id, conversation: conv });
    await until(() => events.some((e) => e.type === "run_done"));
    const done = events.find((e) => e.type === "run_done");
    expect(done?.type === "run_done" && done.run.meta.status).toBe("cancelled");
    // A manual run carries no conversation.
    await until(() => events.some((e) => e.type === "turn_done"));
    const manual = await b.invoke("run_start", { session: info.id, script: "scripts/decay_sim.py", args: [] });
    expect(manual.conversation).toBeNull();
  });

  it("the build-mode proposal has two chunks", async () => {
    const b = mock(0);
    const info = await b.invoke("session_create", { repo_abs: "/home/user/projects/x" });
    const events: SessionEvent[] = [];
    await b.subscribe(info.id, (e) => events.push(e));
    await b.invoke("chat_send", { session: info.id, conversation: info.active_conversation, text: "Add a model" });
    await until(() => events.some((e) => e.type === "proposal"));
    const p = events.find((e) => e.type === "proposal");
    if (p?.type !== "proposal") throw new Error("unreachable");
    expect(p.proposal.proposed).toContain("def double_exp");
    expect(p.proposal.proposed).toContain("(observed minus fit)");
    expect(p.proposal.original).not.toContain("(observed minus fit)");
  });
});

describe("MockBackend additions of IPC.md section 7", () => {
  it("indexes positionals, nargs, const and boolean actions", async () => {
    const b = mock(0);
    const info = await b.invoke("session_create", { repo_abs: "/home/user/projects/x" });
    const events: SessionEvent[] = [];
    await b.subscribe(info.id, (e) => events.push(e));
    await until(() => events.some((e) => e.type === "kernel_state" && e.state === "ready"));
    const index = await b.invoke("index_get", { session: info.id });
    const fit = index.scripts.find((s) => s.name === "fit_decay");
    const by = Object.fromEntries((fit?.options ?? []).map((o) => [o.dest, o]));
    expect(by.input).toMatchObject({ positional: true, flags: ["input"], required: false });
    expect(by.model).toMatchObject({ choices: ["single", "double"], default: "single", positional: false });
    expect(by.label).toMatchObject({ required: true });
    expect(by.bounds).toMatchObject({ nargs: 2, default: [0.1, 10.0], type: "float" });
    expect(by.plot).toMatchObject({ action: "store_false", flags: ["--no-plot"], default: true });
    expect(by.log_scale).toMatchObject({ action: "store_true", default: false });
    expect(by.weights).toMatchObject({ action: "argparse.BooleanOptionalAction", default: true });
    expect(by.verbose).toMatchObject({ action: "count", flags: ["-v", "--verbose"], default: 0 });
    // Existing options gained the new fields too.
    expect(index.scripts.find((s) => s.name === "decay_sim")?.options[0]).toMatchObject({ nargs: null, const: null, positional: false });
  });

  it("provider_keys_status and provider_key_set", async () => {
    const b = mock(0);
    const rows = await b.invoke("provider_keys_status", {});
    const anthropic = () => b.invoke("provider_keys_status", {}).then((r) => r.find((x) => x.provider === "anthropic"));
    expect(rows.find((r) => r.provider === "openai")).toEqual({ provider: "openai", has_key: true, source: "env", api_key_env: "OPENAI_API_KEY" });
    expect(await anthropic()).toEqual({ provider: "anthropic", has_key: false, source: null, api_key_env: "ANTHROPIC_API_KEY" });
    await b.invoke("provider_key_set", { provider: "anthropic", key: "sk-test" });
    expect(await anthropic()).toMatchObject({ has_key: true, source: "stored" });
    // The status never contains the key itself.
    expect(JSON.stringify(await b.invoke("provider_keys_status", {}))).not.toContain("sk-test");
    await b.invoke("provider_key_set", { provider: "anthropic", key: "" });
    expect(await anthropic()).toMatchObject({ has_key: false, source: null });
    // The environment variable takes precedence over a stored key.
    await b.invoke("provider_key_set", { provider: "openai", key: "sk-other" });
    expect((await b.invoke("provider_keys_status", {})).find((r) => r.provider === "openai")?.source).toBe("env");
    await expect(b.invoke("provider_key_set", { provider: "nope", key: "x" })).rejects.toMatchObject({ code: "not_found" });
  });

  it("writes figure sidecars as <file>.meta.json", async () => {
    const b = mock(0);
    await b.invoke("session_open", { session: SEEDED });
    const out = await b.invoke("out_list", { session: SEEDED, path: "out" });
    const paths = out.entries.map((e) => e.path);
    expect(paths.some((p) => p.endsWith("decay-simulation.png.meta.json"))).toBe(true);
    expect(paths.some((p) => p.endsWith("decay-simulation.json"))).toBe(false);
  });
});
