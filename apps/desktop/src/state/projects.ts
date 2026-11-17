import { create } from "zustand";
import type { AppInfo, KernelState, ModelEntry, ProjectId, ProjectInfo, ProjectSummary } from "../ipc";

export interface KernelStatus {
  state: KernelState;
  message: string | null;
}

interface ProjectsState {
  app: AppInfo | null;
  projects: ProjectSummary[];
  infos: Record<ProjectId, ProjectInfo>;
  kernel: Record<ProjectId, KernelStatus>;
  active: ProjectId | null;
  models: ModelEntry[];
  setApp(app: AppInfo): void;
  setProjects(list: ProjectSummary[]): void;
  setInfo(info: ProjectInfo): void;
  setKernel(project: ProjectId, state: KernelState, message: string | null): void;
  setActive(project: ProjectId | null): void;
  setModels(models: ModelEntry[]): void;
  markClosed(project: ProjectId): void;
}

export function summaryOf(x: ProjectSummary): ProjectSummary {
  const { id, name, path_abs, template_name, deck_mode, slides, model, created_at, last_opened_at, open } = x;
  return { id, name, path_abs, template_name, deck_mode, slides, model, created_at, last_opened_at, open };
}

export const useProjects = create<ProjectsState>((set) => ({
  app: null,
  projects: [],
  infos: {},
  kernel: {},
  active: null,
  models: [],
  setApp: (app) => set({ app }),
  setProjects: (projects) => set({ projects }),
  setInfo: (info) =>
    set((s) => {
      const summary = summaryOf(info);
      const exists = s.projects.some((x) => x.id === info.id);
      return {
        infos: { ...s.infos, [info.id]: info },
        projects: exists ? s.projects.map((x) => (x.id === info.id ? summary : x)) : [...s.projects, summary],
        kernel: s.kernel[info.id] ? s.kernel : { ...s.kernel, [info.id]: { state: info.kernel, message: null } },
      };
    }),
  setKernel: (project, state, message) => set((s) => ({ kernel: { ...s.kernel, [project]: { state, message } } })),
  setActive: (active) => set({ active }),
  setModels: (models) => set({ models }),
  markClosed: (project) =>
    set((s) => ({
      projects: s.projects.map((x) => (x.id === project ? { ...x, open: false } : x)),
      kernel: { ...s.kernel, [project]: { state: "stopped", message: null } },
      active: s.active === project ? (s.projects.find((x) => x.open && x.id !== project)?.id ?? null) : s.active,
    })),
}));

export function useActiveProject(): ProjectId | null {
  return useProjects((s) => s.active);
}

export function useProjectInfo(project: ProjectId | null): ProjectInfo | null {
  return useProjects((s) => (project ? (s.infos[project] ?? null) : null));
}

const OFFLINE: KernelStatus = { state: "stopped", message: null };

export function useKernel(project: ProjectId | null): KernelStatus {
  return useProjects((s) => (project ? (s.kernel[project] ?? OFFLINE) : OFFLINE));
}
