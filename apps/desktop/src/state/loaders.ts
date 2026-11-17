// Fetches that bring the stores in line with the backend: the initial load of
// a session, refetches driven by events, and the reconciliation after a
// `resync`. Nothing here waits for the kernel: data that needs none
// (tree, runs, figures, transcripts) loads on its own, and the index follows
// when `kernel_state: ready` or `index_changed` arrives (IPC.md section 7).
import { backend, isIpcError } from "../ipc";
import type { ConversationId, RunId, SessionId, SessionInfo } from "../ipc";
import { flushAgentText, useChat } from "./chat";
import { stateEventSeq } from "./eventSeq";
import { checkExternalChanges } from "./fileWatch";
import { useFiles } from "./files";
import { packagesChanged } from "./packages";
import { discardQueuedOutput, flushRunOutput, useRuns } from "./runs";
import { useSessions } from "./sessions";
import { transcriptToChat } from "./transcript";
import { notifyError } from "./ui";

function kernelUnavailable(e: unknown): boolean {
  return isIpcError(e) && (e.code === "kernel_offline" || e.code === "target_offline");
}

/**
 * Stores a SessionInfo. With `authoritative` (open, create, resync) the kernel
 * state, the busy flags and the pending proposals are taken from it as the
 * truth; a plain `session_changed` only adds what is new.
 */
export function applySessionInfo(info: SessionInfo, authoritative: boolean): void {
  const sessions = useSessions.getState();
  sessions.setInfo(info);
  const chat = useChat.getState();
  if (authoritative) {
    sessions.setKernel(info.id, info.kernel, null);
    for (const c of info.conversations) {
      chat.ensure(c.id);
      if ((useChat.getState().conversations[c.id]?.busy ?? false) !== c.busy) chat.setBusy(c.id, c.busy);
    }
  }
  chat.syncPending(info.id, info.pending_proposals, authoritative);
}

/**
 * Reads `session_info` to reconcile after subscribing or a resync. The answer
 * is applied only if no state-bearing event overtook it; otherwise it is asked
 * for again (a few times at most, then the latest answer is used).
 */
export async function reconcileSessionInfo(session: SessionId): Promise<SessionInfo> {
  for (let attempt = 0; ; attempt++) {
    const seq = stateEventSeq(session);
    const info = await backend().invoke("session_info", { session });
    if (stateEventSeq(session) === seq || attempt >= 4) {
      applySessionInfo(info, true);
      return info;
    }
  }
}

// ---- index ----

const indexInFlight = new Map<SessionId, { promise: Promise<boolean>; again: boolean }>();

/**
 * Refetches the script and library index. Called for `index_changed` and
 * `kernel_state: ready` (and once per load or resync), not for file events.
 * Requests are coalesced: callers during a fetch share it, and however many
 * arrive meanwhile cause one follow-up fetch, since the index may have changed
 * after the request in flight was answered.
 *
 * Returns false when the kernel is not up yet; the index is then marked stale
 * and fetched again on `kernel_state: ready`. Other failures are thrown unless `quiet`.
 */
export function refreshIndex(session: SessionId, quiet = true): Promise<boolean> {
  const running = indexInFlight.get(session);
  if (running) {
    // The index may have changed after the request in flight was answered.
    running.again = true;
    return running.promise;
  }
  const entry = { promise: Promise.resolve(false), again: false };
  entry.promise = (async () => {
    try {
      for (;;) {
        entry.again = false;
        const index = await backend().invoke("index_get", { session });
        if (!entry.again) {
          useFiles.getState().setIndex(session, index);
          return true;
        }
      }
    } catch (e) {
      useFiles.getState().markIndexStale(session);
      if (kernelUnavailable(e) || quiet) return false;
      throw e;
    } finally {
      indexInFlight.delete(session);
    }
  })();
  indexInFlight.set(session, entry);
  return entry.promise;
}

// ---- transcripts ----

const transcriptInFlight = new Map<string, Promise<void>>();

/** Loads the stored transcript of a conversation into chat items (once, unless `replace`). */
export function loadTranscript(session: SessionId, conversation: ConversationId, replace = false): Promise<void> {
  const chat = useChat.getState();
  chat.ensure(conversation);
  if (!replace && useChat.getState().conversations[conversation]?.loaded) return Promise.resolve();
  const key = `${session}\n${conversation}\n${replace ? "r" : ""}`;
  const running = transcriptInFlight.get(key);
  if (running) return running;
  const p = (async () => {
    const events = await backend().invoke("conversation_transcript", { session, conversation });
    const pending = useSessions.getState().infos[session]?.pending_proposals ?? [];
    const mapped = transcriptToChat(session, conversation, events, pending);
    useChat.getState().loadHistory(conversation, mapped.items, mapped.proposals, replace);
  })().finally(() => transcriptInFlight.delete(key));
  transcriptInFlight.set(key, p);
  return p;
}

// ---- file tree ----

export async function loadDir(session: SessionId, dir: string): Promise<void> {
  try {
    const entries = await backend().invoke("fs_tree", { session, path: dir, depth: 1 });
    useFiles.getState().setTree(session, dir, entries);
  } catch (e) {
    if (dir !== "" && isIpcError(e) && e.code === "not_found") {
      // The directory is gone; its parent's refetch removes the row.
      useFiles.setState((s) => {
        const tree = { ...(s.trees[session] ?? {}) };
        delete tree[dir];
        return { trees: { ...s.trees, [session]: tree } };
      });
      return;
    }
    throw e;
  }
}

const dirQueue = new Map<SessionId, { dirs: Set<string>; timer: ReturnType<typeof setTimeout> }>();

/** Re-reads directories shortly, coalescing bursts of `files_changed`. Stale entries stay visible meanwhile. */
export function reloadDirsSoon(session: SessionId, dirs: string[]): void {
  if (dirs.length === 0) return;
  const queued = dirQueue.get(session);
  if (queued) {
    for (const d of dirs) queued.dirs.add(d);
    return;
  }
  const entry = {
    dirs: new Set(dirs),
    timer: setTimeout(() => {
      dirQueue.delete(session);
      if (!useSessions.getState().infos[session]?.open) return;
      for (const dir of entry.dirs) void loadDir(session, dir).catch(() => undefined);
    }, 40),
  };
  dirQueue.set(session, entry);
}

function visibleDirs(session: SessionId): string[] {
  const files = useFiles.getState();
  const loaded = files.trees[session] ?? {};
  const expanded = files.expanded[session] ?? {};
  return Object.keys(loaded).filter((d) => d === "" || expanded[d]);
}

// ---- runs ----

/** Reads a run through `run_get` and takes its status, artifacts and output from the answer. */
export async function reconcileRun(session: SessionId, run_id: RunId): Promise<void> {
  const full = await backend().invoke("run_get", { session, run_id });
  const { stdout, stderr, ...view } = full;
  const runs = useRuns.getState();
  runs.upsertRun(session, view);
  // Chunks still queued are part of the complete text; do not append them again.
  discardQueuedOutput(run_id);
  runs.setOutputText(run_id, stdout, stderr);
}

// ---- session ----

function report(results: PromiseSettledResult<unknown>[]): void {
  const failed = results.find((r): r is PromiseRejectedResult => r.status === "rejected" && !kernelUnavailable(r.reason));
  if (failed) notifyError(failed.reason);
}

/** Initial load after open or create. Each part is independent; one failing does not hold back the others. */
export async function loadSessionData(session: SessionId): Promise<void> {
  const b = backend();
  const info = useSessions.getState().infos[session];
  const results = await Promise.allSettled([
    b.invoke("fs_tree", { session, depth: 1 }).then((tree) => useFiles.getState().setTree(session, "", tree)),
    b.invoke("runs_list", { session, limit: 100 }).then((list) => useRuns.getState().setRuns(session, list)),
    b.invoke("figures_list", { session }).then((groups) => useRuns.getState().setFigures(session, groups)),
    info ? loadTranscript(session, info.active_conversation) : Promise.resolve(),
    refreshIndex(session),
  ]);
  report(results);
}

/**
 * Handles `{ type: "resync" }`: this subscriber fell behind and events were
 * dropped, so everything events would have maintained is read again.
 */
export async function resyncSession(session: SessionId): Promise<void> {
  const b = backend();
  flushRunOutput();
  flushAgentText();
  // A dropped `packages_changed` cannot be told apart from none; ask again when the list is next shown.
  packagesChanged(session);
  const before = useRuns.getState();
  const stale = new Set<RunId>((before.runs[session] ?? []).filter((r) => r.meta.status === "running").map((r) => r.meta.run_id));
  const selected = before.selectedRun[session];
  if (selected) stale.add(selected);

  let info: SessionInfo;
  try {
    info = await reconcileSessionInfo(session);
  } catch (e) {
    notifyError(e);
    return;
  }
  const results = await Promise.allSettled([
    b.invoke("runs_list", { session, limit: 100 }).then((list) => useRuns.getState().setRuns(session, list)),
    b.invoke("figures_list", { session }).then((groups) => useRuns.getState().setFigures(session, groups)),
    loadTranscript(session, info.active_conversation, true),
    refreshIndex(session),
    ...visibleDirs(session).map((dir) => loadDir(session, dir)),
    checkExternalChanges(session, Object.keys(useFiles.getState().buffers[session] ?? {})),
  ]);
  report(results);
  // Other conversations reload from their transcript when they are next shown.
  useChat.setState((s) => {
    const conversations = { ...s.conversations };
    for (const c of info.conversations) {
      const cur = conversations[c.id];
      if (cur && c.id !== info.active_conversation && !cur.busy) conversations[c.id] = { ...cur, items: [], stream: null, loaded: false };
    }
    return { conversations };
  });
  for (const r of useRuns.getState().runs[session] ?? []) if (r.meta.status === "running") stale.add(r.meta.run_id);
  await Promise.allSettled([...stale].map((run_id) => reconcileRun(session, run_id)));
}
