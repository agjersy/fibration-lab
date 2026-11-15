import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RunView } from "../ipc";
import { resetStores } from "../test/util";
import { LruCache, MAX_FIGURES_CACHED } from "./figureCache";
import { EMPTY_OUTPUT, MAX_OUTPUT_CHARS, appendOutput, outputFromText, stripAnsi, toRows } from "./outputText";
import { MAX_RUNS_WITH_OUTPUT, flushRunOutput, queueRunOutput, useRuns } from "./runs";
import { PRE_MOUNT_LIMIT, attachTerminalSink, bufferedTerminalChars, deliverTerminalData, dropTerminal } from "./terminals";

function feed(chunks: string[], max?: number) {
  return chunks.reduce((buf, c) => appendOutput(buf, c, max), EMPTY_OUTPUT);
}

describe("output buffer", () => {
  it("appends plain text", () => {
    expect(feed(["a\n", "b", "c\n"]).text).toBe("a\nbc\n");
  });

  it("a carriage return overwrites the current line (progress bars)", () => {
    expect(feed(["start\n", "\r 10%", "\r 50%", "\r100%", "\ndone\n"]).text).toBe("start\n100%\ndone\n");
    // A shorter rewrite keeps the rest of the line, as a terminal would.
    expect(feed(["abcdef\rXY"]).text).toBe("XYcdef");
    // CR split from its text across chunks.
    expect(feed(["loading\r", "ready  "]).text).toBe("ready  ");
  });

  it("treats CRLF as a newline, also when split across chunks", () => {
    expect(feed(["a\r\nb\r\n"]).text).toBe("a\nb\n");
    expect(feed(["a\r", "\nb"]).text).toBe("a\nb");
  });

  it("strips ANSI SGR and other escape sequences, also when split across chunks", () => {
    expect(stripAnsi("\x1b[1;31mred\x1b[0m plain \x1b[2K\x1b]0;title\x07x").text).toBe("red plain x");
    expect(feed(["ok \x1b[3", "2mgreen\x1b", "[0m!"]).text).toBe("ok green!");
    expect(feed(["a\x1b"]).text).toBe("a");
  });

  it("keeps only the last N characters and counts what was dropped", () => {
    const buf = feed(["0123456789", "abcdefghij", "KLMNO"], 12);
    expect(buf.text).toBe("defghijKLMNO");
    expect(buf.dropped).toBe(13);
    expect(buf.text.endsWith("KLMNO")).toBe(true);
  });

  it("stays bounded at 256 KiB however much is appended", () => {
    const line = `${"x".repeat(99)}\n`;
    let buf = EMPTY_OUTPUT;
    for (let i = 0; i < 60_000; i += 500) buf = appendOutput(buf, line.repeat(500));
    expect(buf.text.length).toBeLessThanOrEqual(MAX_OUTPUT_CHARS);
    expect(buf.text.length).toBeGreaterThan(MAX_OUTPUT_CHARS - 4200);
    expect(buf.dropped + buf.text.length).toBe(6_000_000);
    // Cut at a line boundary.
    expect(buf.text.startsWith("x".repeat(99))).toBe(true);
  });

  it("builds a bounded buffer from complete text", () => {
    const full = Array.from({ length: 50_000 }, (_, i) => `line ${i}`).join("\n");
    const buf = outputFromText(full);
    expect(buf.text.length).toBeLessThanOrEqual(MAX_OUTPUT_CHARS);
    expect(buf.text.endsWith("line 49999")).toBe(true);
    expect(buf.dropped + buf.text.length).toBe(full.length);
    expect(outputFromText("short\n").dropped).toBe(0);
  });

  it("splits 5 MB into rows quickly, breaking very long lines", () => {
    const text = `${"y".repeat(5000)}\n${"line of output number N\n".repeat(220_000)}`;
    expect(text.length).toBeGreaterThan(5_000_000);
    const t0 = performance.now();
    const rows = toRows(text);
    expect(performance.now() - t0).toBeLessThan(1500);
    expect(rows.length).toBe(220_003);
    expect(rows[0]).toHaveLength(2000);
    expect(rows[2]).toHaveLength(1000);
    expect(toRows("a\n\nb\n")).toEqual(["a", "", "b"]);
    expect(toRows("")).toEqual([""]);
  });
});

function run(id: string): RunView {
  return {
    meta: { run_id: id, script: "s.py", args: [], status: "done", exit_code: 0, started_at: "", finished_at: "", out_dir: "", sandbox: "none", tree_hash: null, pid: null, host: "h" },
    target: "local",
    artifacts: [],
    stdout_tail: "",
    stderr_tail: "",
    conversation: null,
  };
}

describe("run output in the store", () => {
  beforeEach(() => {
    resetStores();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("batches chunks: many run_output events become one store update per frame", () => {
    let updates = 0;
    const unsubscribe = useRuns.subscribe(() => updates++);
    for (let i = 0; i < 5000; i++) queueRunOutput("r1", i % 2 ? "stderr" : "stdout", `chunk ${i}\n`);
    expect(updates).toBe(0);
    expect(useRuns.getState().output.r1).toBeUndefined();
    vi.advanceTimersByTime(40);
    flushRunOutput();
    expect(updates).toBe(1);
    const out = useRuns.getState().output.r1;
    expect(out?.stdout.text.startsWith("chunk 0\nchunk 2\n")).toBe(true);
    expect(out?.stderr.text.endsWith("chunk 4999\n")).toBe(true);
    unsubscribe();
  });

  it("caps each stream at 256 KiB and records the truncation", () => {
    const chunk = "z".repeat(1023) + "\n";
    for (let i = 0; i < 5 * 1024; i++) queueRunOutput("big", "stdout", chunk);
    flushRunOutput();
    const out = useRuns.getState().output.big;
    expect(out?.stdout.text.length).toBeLessThanOrEqual(MAX_OUTPUT_CHARS);
    expect(out?.stdout.dropped).toBeGreaterThan(4 * 1024 * 1024);
    expect(out?.stderr.dropped).toBe(0);
  });

  it("keeps output for the 20 most recent runs only, and never evicts the selected run", () => {
    useRuns.getState().selectRun("s", "run0");
    for (let i = 0; i < MAX_RUNS_WITH_OUTPUT + 5; i++) {
      queueRunOutput(`run${i}`, "stdout", "x");
      flushRunOutput();
    }
    const kept = Object.keys(useRuns.getState().output);
    expect(kept).toHaveLength(MAX_RUNS_WITH_OUTPUT);
    expect(kept).toContain("run0");
    expect(kept).not.toContain("run1");
    expect(kept).toContain(`run${MAX_RUNS_WITH_OUTPUT + 4}`);
  });

  it("setOutputText replaces the output with the bounded tail of the full text", () => {
    useRuns.getState().upsertRun("s", run("r"));
    queueRunOutput("r", "stdout", "partial");
    flushRunOutput();
    useRuns.getState().setOutputText("r", "complete\n", "\x1b[31mwarn\x1b[0m\n");
    expect(useRuns.getState().output.r?.stdout.text).toBe("complete\n");
    expect(useRuns.getState().output.r?.stderr.text).toBe("warn\n");
  });
});

describe("figure cache", () => {
  it("is an LRU capped at 64 entries", () => {
    const cache = new LruCache<string>(MAX_FIGURES_CACHED);
    for (let i = 0; i < MAX_FIGURES_CACHED; i++) cache.set(`f${i}`, "data");
    expect(cache.get("f0")).toBe("data"); // touch: now the most recent
    cache.set("new", "data");
    expect(cache.size).toBe(64);
    expect(cache.get("f0")).toBe("data");
    expect(cache.get("f1")).toBeUndefined();
    expect(cache.get("new")).toBe("data");
  });
});

describe("terminal output before the screen exists", () => {
  it("is buffered up to a bound, oldest first out, and flushed to the sink", () => {
    const chunk = "t".repeat(64 * 1024);
    deliverTerminalData("tb", "FIRST");
    for (let i = 0; i < 8; i++) deliverTerminalData("tb", chunk);
    deliverTerminalData("tb", "LAST");
    expect(bufferedTerminalChars("tb")).toBeLessThanOrEqual(PRE_MOUNT_LIMIT);
    const got: string[] = [];
    const detach = attachTerminalSink("tb", (d) => got.push(d));
    const text = got.join("");
    expect(text.endsWith("LAST")).toBe(true);
    expect(text.includes("FIRST")).toBe(false);
    expect(bufferedTerminalChars("tb")).toBe(0);
    detach();
    dropTerminal("tb");
    // Output for a closed terminal is discarded, not buffered forever.
    deliverTerminalData("tb", "late");
    expect(bufferedTerminalChars("tb")).toBe(0);
  });
});
