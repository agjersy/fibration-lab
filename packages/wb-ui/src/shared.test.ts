import { describe, expect, it } from "vitest";
import { createBackendHolder, errorMessage, ipcError, isIpcError } from "./backend";
import { listCommands, registerCommands, runCommand } from "./commands";
import { commandItems, fuzzyScore, rankItems } from "./components/Palette";
import { MockClock, MockEmitter } from "./mock";

describe("backend helpers", () => {
  it("recognises and formats IPC errors", () => {
    const e = ipcError("conflict", "hash mismatch");
    expect(isIpcError(e)).toBe(true);
    expect(errorMessage(e)).toBe("hash mismatch (conflict)");
    expect(errorMessage(new Error("boom"))).toBe("boom");
    expect(isIpcError("nope")).toBe(false);
  });

  it("selects the mock outside Tauri and caches the instance", async () => {
    let loads = 0;
    const holder = createBackendHolder({
      globalKey: "__WB_TEST_BACKEND__",
      loadTauri: async () => ({ kind: "tauri" }),
      loadMock: async () => {
        loads++;
        return { kind: "mock" };
      },
    });
    expect(() => holder.backend()).toThrow();
    expect((await holder.getBackend()).kind).toBe("mock");
    expect((await holder.getBackend()).kind).toBe("mock");
    expect(loads).toBe(1);
    expect(holder.backend().kind).toBe("mock");
  });
});

describe("mock helpers", () => {
  it("fans events out per scope and unsubscribes", () => {
    const em = new MockEmitter<number>();
    const got: number[] = [];
    const un = em.subscribe("a", (n) => got.push(n));
    em.emit("a", 1);
    em.emit("b", 2);
    un();
    em.emit("a", 3);
    expect(got).toEqual([1]);
  });

  it("resolves immediately with zero latency", async () => {
    const clock = new MockClock({ latency: [0, 0], timeScale: 0 });
    await clock.delay();
    await clock.wait(1000);
  });
});

describe("commands and palette ranking", () => {
  it("registers, lists, runs and unregisters commands", () => {
    let ran = 0;
    const un = registerCommands([
      { id: "b", title: "Beta", run: () => void ran++ },
      { id: "a", title: "Alpha", category: "View", run: () => undefined },
      { id: "h", title: "Hidden", hidden: true, run: () => undefined },
    ]);
    expect(listCommands().map((c) => c.id)).toEqual(["a", "b"]);
    expect(runCommand("b")).toBe(true);
    expect(ran).toBe(1);
    expect(runCommand("zzz")).toBe(false);
    expect(commandItems("alp", (id) => (id === "a" ? "Ctrl+A" : null))).toMatchObject([{ id: "a", shortcut: "Ctrl+A", detail: "View" }]);
    un();
    expect(listCommands()).toEqual([]);
  });

  it("scores substrings above subsequences and drops non-matches", () => {
    expect(fuzzyScore("the", "Theme: dark")).toBeGreaterThan(fuzzyScore("tmd", "Theme: dark"));
    expect(fuzzyScore("xyz", "Theme: dark")).toBe(0);
    expect(rankItems("da", ["Theme: dark", "Theme: light", "Data"], (s) => s)).toEqual(["Data", "Theme: dark"]);
  });
});
