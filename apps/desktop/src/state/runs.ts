import { create } from "zustand";
import type { Artifact, FigureGroup, FigureView, RunId, RunView, SessionId } from "../ipc";
import { figureStem } from "./figures";
import { frameBatcher } from "./frame";
import { EMPTY_OUTPUT, appendOutput, outputFromText } from "./outputText";
import type { OutputBuffer } from "./outputText";

export type StreamName = "stdout" | "stderr";

/** Output kept per run: the last 256 KiB of each stream (see outputText.ts). */
export interface RunOutput {
  stdout: OutputBuffer;
  stderr: OutputBuffer;
}

/** Output is kept for this many runs; older entries are dropped and re-read through `run_get` on demand. */
export const MAX_RUNS_WITH_OUTPUT = 20;

export interface FigureSelection {
  /** The figure's stem (file name without extension); one figure may exist in several formats. */
  name: string;
  run_id: RunId;
}

interface RunsState {
  runs: Record<SessionId, RunView[]>;
  output: Record<RunId, RunOutput>;
  /** Run ids with output, least recently touched first. */
  outputOrder: RunId[];
  figures: Record<SessionId, FigureGroup[]>;
  selectedRun: Record<SessionId, RunId | null>;
  /** Figure currently shown in the Figures panel. */
  activeFigure: Record<SessionId, FigureSelection | null>;
  /** Figures pinned so newer runs do not replace them. */
  pinned: Record<SessionId, FigureSelection[]>;
  compare: Record<SessionId, RunId | null>;
  setRuns(session: SessionId, runs: RunView[]): void;
  upsertRun(session: SessionId, run: RunView): void;
  /** Applies a batch of streamed chunks in one store update. */
  appendChunks(batch: ReadonlyMap<RunId, Readonly<Record<StreamName, string>>>): void;
  /** Replaces a run's output with the complete text from `run_get` (the tail is kept). */
  setOutputText(run_id: RunId, stdout: string, stderr: string): void;
  addArtifact(session: SessionId, run_id: RunId, artifact: Artifact, figure: FigureView | null): void;
  setFigures(session: SessionId, groups: FigureGroup[]): void;
  selectRun(session: SessionId, run_id: RunId | null): void;
  setActiveFigure(session: SessionId, selection: FigureSelection | null): void;
  togglePin(session: SessionId, selection: FigureSelection): void;
  setCompare(session: SessionId, run_id: RunId | null): void;
  dropSession(session: SessionId): void;
}

function mergeArtifacts(primary: Artifact[], secondary: Artifact[]): Artifact[] {
  const extra = secondary.filter((a) => !primary.some((b) => b.path === a.path));
  return extra.length ? [...primary, ...extra] : primary;
}

/**
 * Merges an incoming view into the one already known. A finished run is never
 * replaced by a "running" snapshot of itself: events and list results can
 * arrive out of order (a stale `runs_list`, a late `run_started` after a resync).
 */
export function mergeRun(existing: RunView | undefined, incoming: RunView): RunView {
  if (!existing) return incoming;
  if (existing.meta.status !== "running" && incoming.meta.status === "running") {
    return { ...existing, artifacts: mergeArtifacts(existing.artifacts, incoming.artifacts) };
  }
  return { ...incoming, artifacts: mergeArtifacts(incoming.artifacts, existing.artifacts) };
}

function touch(order: RunId[], output: Record<RunId, RunOutput>, ids: RunId[], keep: (id: RunId) => boolean): { order: RunId[]; output: Record<RunId, RunOutput> } {
  let next = order.filter((id) => !ids.includes(id));
  next.push(...ids);
  if (next.length <= MAX_RUNS_WITH_OUTPUT) return { order: next, output };
  const out = { ...output };
  const evictable = next.filter((id) => !ids.includes(id) && !keep(id));
  for (const id of evictable) {
    if (next.length <= MAX_RUNS_WITH_OUTPUT) break;
    delete out[id];
    next = next.filter((x) => x !== id);
  }
  return { order: next, output: out };
}

export const useRuns = create<RunsState>((set) => ({
  runs: {},
  output: {},
  outputOrder: [],
  figures: {},
  selectedRun: {},
  activeFigure: {},
  pinned: {},
  compare: {},
  setRuns: (session, runs) =>
    set((s) => {
      const known = new Map((s.runs[session] ?? []).map((r) => [r.meta.run_id, r]));
      const merged = runs.map((r) => mergeRun(known.get(r.meta.run_id), r));
      // Runs that started after the list was produced are kept in front.
      const listed = new Set(runs.map((r) => r.meta.run_id));
      const newer = (s.runs[session] ?? []).filter((r) => !listed.has(r.meta.run_id) && r.meta.status === "running");
      return { runs: { ...s.runs, [session]: [...newer, ...merged] } };
    }),
  upsertRun: (session, run) =>
    set((s) => {
      const list = s.runs[session] ?? [];
      const idx = list.findIndex((r) => r.meta.run_id === run.meta.run_id);
      const merged = mergeRun(idx >= 0 ? list[idx] : undefined, run);
      const next = idx >= 0 ? list.map((r, i) => (i === idx ? merged : r)) : [merged, ...list];
      const groups = s.figures[session] ?? [];
      const figures = groups.map((g) => (g.run.meta.run_id === run.meta.run_id ? { ...g, run: merged } : g));
      return { runs: { ...s.runs, [session]: next }, figures: { ...s.figures, [session]: figures } };
    }),
  appendChunks: (batch) =>
    set((s) => {
      if (batch.size === 0) return {};
      const output = { ...s.output };
      for (const [run_id, chunks] of batch) {
        const cur = output[run_id] ?? { stdout: EMPTY_OUTPUT, stderr: EMPTY_OUTPUT };
        output[run_id] = {
          stdout: chunks.stdout ? appendOutput(cur.stdout, chunks.stdout) : cur.stdout,
          stderr: chunks.stderr ? appendOutput(cur.stderr, chunks.stderr) : cur.stderr,
        };
      }
      const selected = new Set(Object.values(s.selectedRun));
      const t = touch(s.outputOrder, output, [...batch.keys()], (id) => selected.has(id));
      return { output: t.output, outputOrder: t.order };
    }),
  setOutputText: (run_id, stdout, stderr) =>
    set((s) => {
      const output = { ...s.output, [run_id]: { stdout: outputFromText(stdout), stderr: outputFromText(stderr) } };
      const selected = new Set(Object.values(s.selectedRun));
      const t = touch(s.outputOrder, output, [run_id], (id) => selected.has(id));
      return { output: t.output, outputOrder: t.order };
    }),
  addArtifact: (session, run_id, artifact, figure) =>
    set((s) => {
      const list = s.runs[session] ?? [];
      const runs = list.map((r) =>
        r.meta.run_id === run_id && !r.artifacts.some((a) => a.path === artifact.path) ? { ...r, artifacts: [...r.artifacts, artifact] } : r,
      );
      if (!figure) return { runs: { ...s.runs, [session]: runs } };
      const run = runs.find((r) => r.meta.run_id === run_id);
      const groups = s.figures[session] ?? [];
      const existing = groups.find((g) => g.run.meta.run_id === run_id);
      let figures: FigureGroup[];
      if (existing) {
        figures = groups.map((g) =>
          g.run.meta.run_id === run_id ? { ...g, figures: g.figures.some((f) => f.path === figure.path) ? g.figures : [...g.figures, figure] } : g,
        );
      } else if (run) {
        figures = [{ run, figures: [figure] }, ...groups];
      } else {
        figures = groups;
      }
      const pinned = s.pinned[session] ?? [];
      const active = s.activeFigure[session];
      const keepActive = active !== null && active !== undefined && pinned.some((p) => p.name === active.name && p.run_id === active.run_id);
      const activeFigure = keepActive ? active : { name: figureStem(figure), run_id };
      return {
        runs: { ...s.runs, [session]: runs },
        figures: { ...s.figures, [session]: figures },
        activeFigure: { ...s.activeFigure, [session]: activeFigure },
      };
    }),
  setFigures: (session, groups) =>
    set((s) => {
      // Keep the freshest view of each run (see mergeRun) and any group that
      // streamed in for a run the list does not know yet.
      const runs = new Map((s.runs[session] ?? []).map((r) => [r.meta.run_id, r]));
      const merged = groups.map((g) => ({ ...g, run: mergeRun(runs.get(g.run.meta.run_id), g.run) }));
      const listed = new Set(groups.map((g) => g.run.meta.run_id));
      const live = (s.figures[session] ?? []).filter((g) => !listed.has(g.run.meta.run_id) && g.run.meta.status === "running");
      const all = [...live, ...merged];
      const first = all[0]?.figures[0];
      const active = s.activeFigure[session] ?? (first ? { name: figureStem(first), run_id: first.run_id } : null);
      return { figures: { ...s.figures, [session]: all }, activeFigure: { ...s.activeFigure, [session]: active } };
    }),
  selectRun: (session, run_id) => set((s) => ({ selectedRun: { ...s.selectedRun, [session]: run_id } })),
  setActiveFigure: (session, selection) => set((s) => ({ activeFigure: { ...s.activeFigure, [session]: selection } })),
  togglePin: (session, selection) =>
    set((s) => {
      const list = s.pinned[session] ?? [];
      const has = list.some((p) => p.name === selection.name && p.run_id === selection.run_id);
      return {
        pinned: {
          ...s.pinned,
          [session]: has ? list.filter((p) => !(p.name === selection.name && p.run_id === selection.run_id)) : [...list, selection],
        },
      };
    }),
  setCompare: (session, run_id) => set((s) => ({ compare: { ...s.compare, [session]: run_id } })),
  dropSession: (session) =>
    set((s) => {
      const ids = new Set((s.runs[session] ?? []).map((r) => r.meta.run_id));
      const output = Object.fromEntries(Object.entries(s.output).filter(([id]) => !ids.has(id)));
      return { output, outputOrder: s.outputOrder.filter((id) => !ids.has(id)) };
    }),
}));

export function findRun(session: SessionId, run_id: RunId): RunView | undefined {
  return useRuns.getState().runs[session]?.find((r) => r.meta.run_id === run_id);
}

// ---- streamed output batching ----
// `run_output` events can arrive thousands of times per second. They are
// collected here and applied once per animation frame, so the store (and the
// output panel) update at most once per frame however chatty the script is.

const pending = new Map<RunId, Record<StreamName, string[]>>();
const outputFrame = frameBatcher(() => flushRunOutput());

export function queueRunOutput(run_id: RunId, stream: StreamName, chunk: string): void {
  let entry = pending.get(run_id);
  if (!entry) {
    entry = { stdout: [], stderr: [] };
    pending.set(run_id, entry);
  }
  entry[stream].push(chunk);
  outputFrame.schedule();
}

/** Applies everything queued so far. Called per frame, and before anything that reads or replaces output. */
export function flushRunOutput(): void {
  outputFrame.cancel();
  if (pending.size === 0) return;
  const batch = new Map<RunId, Record<StreamName, string>>();
  for (const [run_id, chunks] of pending) batch.set(run_id, { stdout: chunks.stdout.join(""), stderr: chunks.stderr.join("") });
  pending.clear();
  useRuns.getState().appendChunks(batch);
}

/** Drops queued chunks for a run whose output is about to be replaced wholesale. */
export function discardQueuedOutput(run_id: RunId): void {
  pending.delete(run_id);
}
