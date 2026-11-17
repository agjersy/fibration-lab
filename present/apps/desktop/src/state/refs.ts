import { create } from "zustand";
import type { ProjectId, RefView, SourcePassage } from "../ipc";

export interface SourceLens {
  cite: string;
  /** null while loading. */
  passage: SourcePassage | null;
  error: string | null;
}

interface RefsState {
  refs: Record<ProjectId, RefView[]>;
  /** The citation the Sources panel shows, per project. */
  source: Record<ProjectId, SourceLens | null>;
  setRefs(project: ProjectId, refs: RefView[]): void;
  setSource(project: ProjectId, source: SourceLens | null): void;
}

export const useRefs = create<RefsState>((set) => ({
  refs: {},
  source: {},
  setRefs: (project, refs) => set((s) => ({ refs: { ...s.refs, [project]: refs } })),
  setSource: (project, source) => set((s) => ({ source: { ...s.source, [project]: source } })),
}));
