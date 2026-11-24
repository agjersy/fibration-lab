import { create } from "zustand";
import type { FileEntry, IndexResult, SessionId } from "../ipc";

export type LineSeparator = "\n" | "\r\n";

/**
 * The line ending a file uses, so the editor can keep it. Mixed files take the
 * majority; the minority endings stay in the text untouched.
 */
export function detectLineSeparator(content: string): LineSeparator {
  let crlf = 0;
  let lf = 0;
  for (let i = content.indexOf("\n"); i >= 0; i = content.indexOf("\n", i + 1)) {
    if (i > 0 && content.charCodeAt(i - 1) === 13) crlf++;
    else lf++;
  }
  return crlf > lf ? "\r\n" : "\n";
}

export interface EditorBuffer {
  path: string;
  /**
   * The text handed to the editor when it (re)creates its state: what was
   * loaded or last saved, or the unsaved text of an editor that was torn down.
   * It is NOT kept up to date while typing; the live text is in the editor and
   * is read through `bufferText` / `snapshotBuffer` when someone needs it.
   */
  content: string;
  /** Text as last loaded from or written to disk. */
  saved: string;
  /** Line ending of the file on disk; the editor state is configured with it. */
  lineSeparator: LineSeparator;
  /** sha256 of `saved` as reported by the backend. */
  hash: string;
  /** Reported by the editor from its own change tracking (document differs from the saved one). */
  dirty: boolean;
  /** Set when `files_changed` touched this path while the buffer is clean or dirty. */
  external: "none" | "changed" | "deleted";
  loading: boolean;
  error: string | null;
  /** Bumped when the store replaces the text so the editor reloads it. */
  revision: number;
}

export interface RevealRequest {
  path: string;
  line: number | null;
  nonce: number;
}

interface FilesState {
  /** Directory children keyed by directory path ("" = repo root). */
  trees: Record<SessionId, Record<string, FileEntry[]>>;
  expanded: Record<SessionId, Record<string, boolean>>;
  buffers: Record<SessionId, Record<string, EditorBuffer>>;
  index: Record<SessionId, IndexResult | null>;
  indexStale: Record<SessionId, boolean>;
  activeFile: Record<SessionId, string | null>;
  reveal: Record<SessionId, RevealRequest | null>;
  setTree(session: SessionId, dir: string, entries: FileEntry[]): void;
  setExpanded(session: SessionId, dir: string, expanded: boolean): void;
  /**
   * Called for `files_changed`. Returns the loaded directories that are visible
   * (the root and expanded folders) and must be re-read; their stale entries
   * stay in place until the refetch lands. Collapsed directories are dropped
   * and load again when expanded.
   */
  invalidateDirs(session: SessionId, paths: string[]): string[];
  setBuffer(session: SessionId, buffer: EditorBuffer): void;
  loadingBuffer(session: SessionId, path: string): void;
  bufferError(session: SessionId, path: string, error: string): void;
  /** The editor's document started or stopped differing from the saved text. */
  setDirty(session: SessionId, path: string, dirty: boolean): void;
  /** Keeps the unsaved text of an editor that is going away, for the next one to pick up. */
  stashContent(session: SessionId, path: string, content: string): void;
  /** `written` is the exact text the backend stored; `dirty` says whether the editor has moved on since it was read. */
  markSaved(session: SessionId, path: string, hash: string, written: string, dirty: boolean): void;
  markExternal(session: SessionId, paths: string[], kind: "changed" | "deleted"): void;
  clearExternal(session: SessionId, path: string): void;
  closeBuffer(session: SessionId, path: string): void;
  renameBuffer(session: SessionId, from: string, to: string): void;
  setIndex(session: SessionId, index: IndexResult): void;
  markIndexStale(session: SessionId): void;
  setActiveFile(session: SessionId, path: string | null): void;
  requestReveal(session: SessionId, path: string, line: number | null): void;
  dropSession(session: SessionId): void;
}

/** Ancestor directories of a path, nearest first, ending with the root "". */
export function ancestorsOf(path: string): string[] {
  const out: string[] = [];
  let dir = dirOf(path);
  for (;;) {
    out.push(dir);
    if (dir === "") return out;
    dir = dirOf(dir);
  }
}

export function dirOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

export const useFiles = create<FilesState>((set, get) => ({
  trees: {},
  expanded: {},
  buffers: {},
  index: {},
  indexStale: {},
  activeFile: {},
  reveal: {},
  setTree: (session, dir, entries) =>
    set((s) => ({ trees: { ...s.trees, [session]: { ...(s.trees[session] ?? {}), [dir]: entries } } })),
  setExpanded: (session, dir, expanded) =>
    set((s) => ({ expanded: { ...s.expanded, [session]: { ...(s.expanded[session] ?? {}), [dir]: expanded } } })),
  invalidateDirs: (session, paths) => {
    const s = get();
    const loaded = s.trees[session] ?? {};
    const expanded = s.expanded[session] ?? {};
    const reload = new Set<string>();
    const drop = new Set<string>();
    for (const p of paths) {
      // The path itself may be a directory (created, removed, renamed).
      for (const dir of [p, ...ancestorsOf(p)]) {
        if (loaded[dir] === undefined) continue;
        if (dir === "" || expanded[dir]) reload.add(dir);
        else drop.add(dir);
      }
    }
    if (drop.size > 0) {
      const tree = { ...loaded };
      for (const dir of drop) delete tree[dir];
      set({ trees: { ...s.trees, [session]: tree } });
    }
    return [...reload];
  },
  setBuffer: (session, buffer) =>
    set((s) => ({ buffers: { ...s.buffers, [session]: { ...(s.buffers[session] ?? {}), [buffer.path]: buffer } } })),
  loadingBuffer: (session, path) =>
    set((s) => {
      const existing = s.buffers[session]?.[path];
      const buffer: EditorBuffer = existing
        ? { ...existing, loading: true }
        : { path, content: "", saved: "", lineSeparator: "\n", hash: "", dirty: false, external: "none", loading: true, error: null, revision: 0 };
      return { buffers: { ...s.buffers, [session]: { ...(s.buffers[session] ?? {}), [path]: buffer } } };
    }),
  bufferError: (session, path, error) =>
    set((s) => {
      const existing = s.buffers[session]?.[path];
      if (!existing) return {};
      return { buffers: { ...s.buffers, [session]: { ...s.buffers[session], [path]: { ...existing, loading: false, error } } } };
    }),
  setDirty: (session, path, dirty) =>
    set((s) => {
      const existing = s.buffers[session]?.[path];
      // Returning the state itself tells zustand nothing changed: no subscriber is notified.
      if (!existing || existing.dirty === dirty) return s;
      return { buffers: { ...s.buffers, [session]: { ...s.buffers[session], [path]: { ...existing, dirty } } } };
    }),
  stashContent: (session, path, content) =>
    set((s) => {
      const existing = s.buffers[session]?.[path];
      if (!existing || existing.content === content) return s;
      return { buffers: { ...s.buffers, [session]: { ...s.buffers[session], [path]: { ...existing, content } } } };
    }),
  markSaved: (session, path, hash, written, dirty) =>
    set((s) => {
      const existing = s.buffers[session]?.[path];
      if (!existing) return {};
      return {
        buffers: {
          ...s.buffers,
          [session]: { ...s.buffers[session], [path]: { ...existing, content: written, saved: written, hash, dirty, external: "none" } },
        },
      };
    }),
  markExternal: (session, paths, kind) =>
    set((s) => {
      const bufs = s.buffers[session];
      if (!bufs) return {};
      let changed = false;
      const next = { ...bufs };
      for (const p of paths) {
        const b = next[p];
        if (b) {
          next[p] = { ...b, external: kind };
          changed = true;
        }
      }
      return changed ? { buffers: { ...s.buffers, [session]: next } } : {};
    }),
  clearExternal: (session, path) =>
    set((s) => {
      const b = s.buffers[session]?.[path];
      if (!b) return {};
      return { buffers: { ...s.buffers, [session]: { ...s.buffers[session], [path]: { ...b, external: "none" } } } };
    }),
  closeBuffer: (session, path) =>
    set((s) => {
      const bufs = { ...(s.buffers[session] ?? {}) };
      delete bufs[path];
      return { buffers: { ...s.buffers, [session]: bufs } };
    }),
  renameBuffer: (session, from, to) =>
    set((s) => {
      const bufs = { ...(s.buffers[session] ?? {}) };
      const b = bufs[from];
      if (!b) return {};
      delete bufs[from];
      // The editor at the old path closes with the rename; its text moves along.
      bufs[to] = { ...b, path: to, content: liveSnapshot(session, from)?.text ?? b.content };
      return { buffers: { ...s.buffers, [session]: bufs } };
    }),
  setIndex: (session, index) =>
    set((s) => ({ index: { ...s.index, [session]: index }, indexStale: { ...s.indexStale, [session]: false } })),
  markIndexStale: (session) => set((s) => ({ indexStale: { ...s.indexStale, [session]: true } })),
  setActiveFile: (session, path) => set((s) => ({ activeFile: { ...s.activeFile, [session]: path } })),
  requestReveal: (session, path, line) =>
    set((s) => ({ reveal: { ...s.reveal, [session]: { path, line, nonce: (s.reveal[session]?.nonce ?? 0) + 1 } } })),
  dropSession: (session) =>
    set((s) => {
      const without = <T,>(rec: Record<SessionId, T>): Record<SessionId, T> => {
        const next = { ...rec };
        delete next[session];
        return next;
      };
      return {
        trees: without(s.trees),
        expanded: without(s.expanded),
        buffers: without(s.buffers),
        index: without(s.index),
        indexStale: without(s.indexStale),
        activeFile: without(s.activeFile),
        reveal: without(s.reveal),
      };
    }),
}));

// ---- live editor text ----
// The store does not mirror the editor's text. An open editor registers a
// source here; consumers that need the text (save, run, rename) ask for a
// snapshot, which is the only time the document is serialised.

export interface TextSnapshot {
  text: string;
  /**
   * Tells the editor this snapshot is now what is on disk. Returns whether the
   * editor's document differs from it (text typed while the save was in flight).
   */
  markSaved(): boolean;
}

/** Returns null while the editor does not hold the buffer's text (still loading or reloading). */
export type TextSource = () => TextSnapshot | null;

const textSources = new Map<string, TextSource>();
const sourceKey = (session: SessionId, path: string) => `${session}\n${path}`;

export function registerTextSource(session: SessionId, path: string, source: TextSource): () => void {
  const key = sourceKey(session, path);
  textSources.set(key, source);
  return () => {
    if (textSources.get(key) === source) textSources.delete(key);
  };
}

function liveSnapshot(session: SessionId, path: string): TextSnapshot | null {
  return textSources.get(sourceKey(session, path))?.() ?? null;
}

/** The buffer's current text: from the open editor, else what the store holds. Null without a buffer. */
export function snapshotBuffer(session: SessionId, path: string): TextSnapshot | null {
  const buffer = useFiles.getState().buffers[session]?.[path];
  if (!buffer) return null;
  return liveSnapshot(session, path) ?? { text: buffer.content, markSaved: () => false };
}

export function bufferText(session: SessionId, path: string): string | null {
  return snapshotBuffer(session, path)?.text ?? null;
}

export function dirtyPaths(session: SessionId): string[] {
  return Object.values(useFiles.getState().buffers[session] ?? {})
    .filter((b) => b.dirty)
    .map((b) => b.path);
}

export function anyDirty(): boolean {
  return Object.values(useFiles.getState().buffers).some((bufs) => Object.values(bufs).some((b) => b.dirty));
}

export function useBuffer(session: SessionId | null, path: string | null): EditorBuffer | undefined {
  return useFiles((s) => (session && path ? s.buffers[session]?.[path] : undefined));
}

export function useIndex(session: SessionId | null): IndexResult | null {
  return useFiles((s) => (session ? (s.index[session] ?? null) : null));
}
