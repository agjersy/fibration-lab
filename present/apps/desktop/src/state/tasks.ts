// Ported from Personal Analyst apps/desktop/src/state/tasks.ts at commit f229e4d; `task_log` became
// `task_progress` (a fraction and a message).
import { create } from "zustand";
import type { ProjectId, TaskId } from "../ipc";

export interface TaskRecord {
  id: TaskId;
  project: ProjectId;
  label: string;
  fraction: number | null;
  log: string[];
  done: boolean;
  ok: boolean | null;
  message: string | null;
  started_at: number;
}

interface TasksState {
  tasks: Record<TaskId, TaskRecord>;
  start(project: ProjectId, id: TaskId, label: string): void;
  progress(project: ProjectId, id: TaskId, fraction: number | null, message: string): void;
  done(project: ProjectId, id: TaskId, ok: boolean, message: string | null): void;
}

const blank = (project: ProjectId, id: TaskId): TaskRecord => ({ id, project, label: id, fraction: null, log: [], done: false, ok: null, message: null, started_at: Date.now() });

export const useTasks = create<TasksState>((set) => ({
  tasks: {},
  start: (project, id, label) => set((s) => ({ tasks: { ...s.tasks, [id]: { ...(s.tasks[id] ?? blank(project, id)), label, project } } })),
  progress: (project, id, fraction, message) =>
    set((s) => {
      const t = s.tasks[id] ?? blank(project, id);
      return { tasks: { ...s.tasks, [id]: { ...t, fraction, log: [...t.log, message] } } };
    }),
  done: (project, id, ok, message) =>
    set((s) => {
      const t = s.tasks[id] ?? blank(project, id);
      return { tasks: { ...s.tasks, [id]: { ...t, done: true, ok, message, fraction: ok ? 1 : t.fraction } } };
    }),
}));
