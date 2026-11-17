import { create } from "zustand";
import type { Diagnostic, ProjectId } from "../ipc";

interface ChecksState {
  /** Diagnostics per project, keyed by slide id ("" for deck-level findings). */
  bySlide: Record<ProjectId, Record<string, Diagnostic[]>>;
  /** `diagnostics` event: replaces the diagnostics of the listed slides. */
  replace(project: ProjectId, slides: string[], diagnostics: Diagnostic[]): void;
  setAll(project: ProjectId, diagnostics: Diagnostic[]): void;
}

function group(diagnostics: Diagnostic[]): Record<string, Diagnostic[]> {
  const out: Record<string, Diagnostic[]> = {};
  for (const d of diagnostics) (out[d.slide_id ?? ""] ??= []).push(d);
  return out;
}

export const useChecks = create<ChecksState>((set) => ({
  bySlide: {},
  replace: (project, slides, diagnostics) =>
    set((s) => {
      const next = { ...(s.bySlide[project] ?? {}) };
      for (const id of slides) delete next[id];
      for (const [id, list] of Object.entries(group(diagnostics))) next[id] = [...(next[id] ?? []), ...list];
      return { bySlide: { ...s.bySlide, [project]: next } };
    }),
  setAll: (project, diagnostics) => set((s) => ({ bySlide: { ...s.bySlide, [project]: group(diagnostics) } })),
}));

export function useProjectDiagnostics(project: ProjectId | null): Diagnostic[] {
  const by = useChecks((s) => (project ? s.bySlide[project] : undefined));
  return by ? Object.values(by).flat() : [];
}
