import { create } from "zustand";
import type { SessionId, TerminalId } from "../ipc";

export interface TerminalTab {
  id: TerminalId;
  title: string;
  exit: number | null | undefined; // undefined = running
}

interface TerminalsState {
  terminals: Record<SessionId, TerminalTab[]>;
  active: Record<SessionId, TerminalId | null>;
  /** Next "Terminal N" number per session, so titles are not reused after a close. */
  counter: Record<SessionId, number>;
  add(session: SessionId, id: TerminalId): void;
  remove(session: SessionId, id: TerminalId): void;
  setActive(session: SessionId, id: TerminalId | null): void;
  exited(id: TerminalId, code: number | null): void;
  clearSession(session: SessionId): TerminalId[];
}

export const useTerminals = create<TerminalsState>((set, get) => ({
  terminals: {},
  active: {},
  counter: {},
  add: (session, id) =>
    set((s) => {
      const n = (s.counter[session] ?? 0) + 1;
      return {
        terminals: { ...s.terminals, [session]: [...(s.terminals[session] ?? []), { id, title: `Terminal ${n}`, exit: undefined }] },
        active: { ...s.active, [session]: id },
        counter: { ...s.counter, [session]: n },
      };
    }),
  remove: (session, id) =>
    set((s) => {
      const list = (s.terminals[session] ?? []).filter((t) => t.id !== id);
      const active = s.active[session] === id ? (list[list.length - 1]?.id ?? null) : (s.active[session] ?? null);
      return { terminals: { ...s.terminals, [session]: list }, active: { ...s.active, [session]: active } };
    }),
  setActive: (session, id) => set((s) => ({ active: { ...s.active, [session]: id } })),
  exited: (id, code) =>
    set((s) => {
      const terminals: Record<SessionId, TerminalTab[]> = {};
      for (const [session, list] of Object.entries(s.terminals)) {
        terminals[session] = list.map((t) => (t.id === id ? { ...t, exit: code } : t));
      }
      return { terminals };
    }),
  clearSession: (session) => {
    const ids = (get().terminals[session] ?? []).map((t) => t.id);
    set((s) => ({ terminals: { ...s.terminals, [session]: [] }, active: { ...s.active, [session]: null } }));
    return ids;
  },
}));

// Terminal byte streams bypass React state. Each terminal id has one xterm
// instance that lives for as long as the terminal does (see ui/bottom/xtermRegistry.ts)
// and registers a sink here. Output that arrives before the instance exists is
// held in a bounded buffer: the oldest chunks are dropped past PRE_MOUNT_LIMIT.

export const PRE_MOUNT_LIMIT = 256 * 1024;

const sinks = new Map<TerminalId, (data: string) => void>();
const buffered = new Map<TerminalId, { chunks: string[]; size: number }>();
const disposers = new Map<TerminalId, () => void>();
/** Closed terminals; late output for them is discarded instead of buffered. */
const gone = new Set<TerminalId>();

export function attachTerminalSink(id: TerminalId, sink: (data: string) => void): () => void {
  sinks.set(id, sink);
  const pending = buffered.get(id);
  if (pending) {
    buffered.delete(id);
    for (const chunk of pending.chunks) sink(chunk);
  }
  return () => {
    if (sinks.get(id) === sink) sinks.delete(id);
  };
}

export function deliverTerminalData(id: TerminalId, data: string): void {
  const sink = sinks.get(id);
  if (sink) {
    sink(data);
    return;
  }
  if (gone.has(id)) return;
  const entry = buffered.get(id) ?? { chunks: [], size: 0 };
  entry.chunks.push(data);
  entry.size += data.length;
  while (entry.size > PRE_MOUNT_LIMIT && entry.chunks.length > 1) {
    const first = entry.chunks.shift();
    entry.size -= first?.length ?? 0;
  }
  if (entry.size > PRE_MOUNT_LIMIT) {
    const only = entry.chunks[0] ?? "";
    entry.chunks[0] = only.slice(only.length - PRE_MOUNT_LIMIT);
    entry.size = PRE_MOUNT_LIMIT;
  }
  buffered.set(id, entry);
}

/** Registered by the xterm registry so closing a terminal also disposes its screen. */
export function registerTerminalDisposer(id: TerminalId, dispose: () => void): void {
  disposers.set(id, dispose);
}

/** Releases everything held for a terminal that is gone for good. */
export function dropTerminal(id: TerminalId): void {
  gone.add(id);
  sinks.delete(id);
  buffered.delete(id);
  const dispose = disposers.get(id);
  disposers.delete(id);
  dispose?.();
}

export function bufferedTerminalChars(id: TerminalId): number {
  return buffered.get(id)?.size ?? 0;
}
