// Async orchestration on top of the stores: every call goes through the Backend.
import { backend, errorMessage, getBackend, isIpcError } from "../ipc";
import type { Attachment, ConversationId, ProposalDecision, ProposalId, SessionId } from "../ipc";
import { useChat } from "./chat";
import { isSubscribed, subscribeSession, unsubscribeSession } from "./dispatcher";
import { dropSessionFigures } from "./figureCache";
import { detectLineSeparator, dirtyPaths, useFiles } from "./files";
import { applySessionInfo, loadSessionData, loadTranscript, reconcileRun, reconcileSessionInfo, refreshIndex as refreshIndexQuiet } from "./loaders";
import { useRuns } from "./runs";
import { useSessions } from "./sessions";
import { useTasks } from "./tasks";
import { dropTerminal, useTerminals } from "./terminals";
import { notifyError, useUi } from "./ui";

export { loadDir, loadSessionData } from "./loaders";
export { ensureFigureData } from "./figureCache";

export async function bootstrap(): Promise<void> {
  const b = await getBackend();
  const sessions = useSessions.getState();
  const [app, models, targets, list] = await Promise.all([
    b.invoke("app_info", {}),
    b.invoke("models_list", {}),
    b.invoke("targets_list", {}),
    b.invoke("sessions_list", {}),
  ]);
  sessions.setApp(app);
  sessions.setModels(models);
  sessions.setTargets(targets);
  sessions.setSessions(list);
  // Sessions the backend still has open (a reloaded window): attach to each.
  const open = list.filter((s) => s.open);
  const results = await Promise.allSettled(open.map((s) => openSession(s.id, false)));
  for (const r of results) if (r.status === "rejected") notifyError(r.reason);
  const first = open.find((_, i) => results[i]?.status === "fulfilled");
  if (first) sessions.setActive(first.id);
}

const opening = new Map<SessionId, Promise<void>>();

/**
 * Opens (or re-attaches to) a session. The order is the contract of IPC.md
 * section 7: `session_open`, then `session_subscribe`, then `session_info` to
 * pick up whatever happened between the two. Nothing here waits for the
 * kernel; the session's data loads in the background.
 */
export function openSession(session: SessionId, activate = true): Promise<void> {
  const sessions = useSessions.getState();
  if (sessions.infos[session] && isSubscribed(session) && !opening.has(session)) {
    if (activate) sessions.setActive(session);
    return Promise.resolve();
  }
  const running = opening.get(session);
  if (running) return running;
  const p = (async () => {
    const opened = await backend().invoke("session_open", { session });
    applySessionInfo(opened, true);
    await subscribeSession(session);
    await reconcileSessionInfo(session);
    if (activate) useSessions.getState().setActive(session);
    void loadSessionData(session);
  })().finally(() => opening.delete(session));
  opening.set(session, p);
  return p;
}

export async function createSession(args: { repo_abs: string; target?: string; model?: string; init?: boolean }): Promise<SessionId> {
  // `session_create` opens the session; from here on it exists, so a problem
  // attaching to it is reported but is not a failed creation.
  const info = await backend().invoke("session_create", args);
  applySessionInfo(info, true);
  useSessions.getState().setActive(info.id);
  // Attaching and loading continue in the background; the caller (the dialog) is done.
  void (async () => {
    try {
      await subscribeSession(info.id);
      await reconcileSessionInfo(info.id);
    } catch (e) {
      useUi.getState().toast("error", `Session created, but its event channel failed: ${errorMessage(e)}. Select the session to retry.`);
    }
    await loadSessionData(info.id);
  })();
  return info.id;
}

/** Asks about unsaved buffers. Resolves true when it is fine to go on (saved or discarded). */
export async function confirmUnsaved(session: SessionId, paths: string[], action: string): Promise<boolean> {
  if (paths.length === 0) return true;
  const list = paths.length <= 4 ? paths.join(", ") : `${paths.slice(0, 4).join(", ")} and ${paths.length - 4} more`;
  const choice = await useUi.getState().askUnsaved(action, `Unsaved changes in ${list}.`);
  if (choice === "cancel") return false;
  if (choice === "discard") return true;
  for (const path of paths) {
    if (!(await saveBuffer(session, path))) return false;
  }
  return true;
}

/** Closes a session after the unsaved-changes guard. Resolves false when the user cancelled. */
export async function closeSession(session: SessionId): Promise<boolean> {
  if (!(await confirmUnsaved(session, dirtyPaths(session), "Close session"))) return false;
  const conversations = useSessions.getState().infos[session]?.conversations.map((c) => c.id) ?? [];
  await backend().invoke("session_close", { session });
  // The backend ended the subscription with the session; this only forgets it locally
  // (the redundant unsubscribe error, if any, is swallowed).
  unsubscribeSession(session);
  useSessions.getState().markClosed(session);
  for (const id of useTerminals.getState().clearSession(session)) dropTerminal(id);
  useFiles.getState().dropSession(session);
  useRuns.getState().dropSession(session);
  dropSessionFigures(session);
  useChat.getState().forget(conversations);
  return true;
}

/** Manual index refresh: reports failures, including a kernel that is not up. */
export async function refreshIndex(session: SessionId): Promise<void> {
  const ok = await refreshIndexQuiet(session, false);
  if (!ok) useUi.getState().toast("info", "The index needs the kernel; it will load when the kernel is ready.");
}

export async function loadBuffer(session: SessionId, path: string, force = false): Promise<void> {
  const files = useFiles.getState();
  const existing = files.buffers[session]?.[path];
  if (existing && !force && !existing.error) return;
  files.loadingBuffer(session, path);
  try {
    const fc = await backend().invoke("fs_read", { session, path });
    const cur = useFiles.getState().buffers[session]?.[path];
    files.setBuffer(session, {
      path,
      content: fc.content,
      saved: fc.content,
      lineSeparator: detectLineSeparator(fc.content),
      hash: fc.hash,
      dirty: false,
      external: "none",
      loading: false,
      error: null,
      revision: (cur?.revision ?? 0) + 1,
    });
  } catch (e) {
    files.bufferError(session, path, errorMessage(e));
    throw e;
  }
}

const saveChains = new Map<string, Promise<boolean>>();

/**
 * Saves a buffer. Saves of one path run one after another, each writing the
 * text current when it starts and sending the hash the previous one produced.
 * The buffer is marked saved with the content actually written, so text typed
 * while the write was in flight stays dirty. Resolves false when nothing was written.
 */
export function saveBuffer(session: SessionId, path: string, overwrite = false): Promise<boolean> {
  const key = `${session}\n${path}`;
  const prev = saveChains.get(key) ?? Promise.resolve(true);
  const next = prev.then(
    () => writeBuffer(session, path, overwrite),
    () => writeBuffer(session, path, overwrite),
  );
  saveChains.set(key, next);
  void next.finally(() => {
    if (saveChains.get(key) === next) saveChains.delete(key);
  });
  return next;
}

async function writeBuffer(session: SessionId, path: string, overwrite: boolean): Promise<boolean> {
  const buffer = useFiles.getState().buffers[session]?.[path];
  if (!buffer || buffer.loading) return false;
  // A queued save finds the buffer already written by the one before it.
  if (!buffer.dirty && !overwrite && buffer.external === "none") return true;
  const content = buffer.content;
  try {
    const res = await backend().invoke("fs_write", {
      session,
      path,
      content,
      ...(overwrite || !buffer.hash ? {} : { expected_hash: buffer.hash }),
    });
    useFiles.getState().markSaved(session, path, res.hash, content);
    return true;
  } catch (e) {
    if (isIpcError(e) && e.code === "conflict") {
      useFiles.getState().markExternal(session, [path], "changed");
      useUi.getState().toast("error", `${path} changed on disk; reload or overwrite from the banner.`);
      return false;
    }
    notifyError(e);
    return false;
  }
}

export async function runScript(session: SessionId, script: string, args: string[]): Promise<string | null> {
  try {
    const run = await backend().invoke("run_start", { session, script, args });
    useRuns.getState().upsertRun(session, run);
    useRuns.getState().selectRun(session, run.meta.run_id);
    return run.meta.run_id;
  } catch (e) {
    notifyError(e);
    return null;
  }
}

/** Saves the script first when it has unsaved edits; the run does not start if the save fails. */
export async function runCurrentScript(session: SessionId, path: string): Promise<string | null> {
  const buffer = useFiles.getState().buffers[session]?.[path];
  if (buffer?.dirty && !(await saveBuffer(session, path))) {
    useUi.getState().toast("error", `Not running ${path}: saving it failed.`);
    return null;
  }
  return runScript(session, path, []);
}

/** Loads a run's output (bounded tail) and current state through `run_get`. */
export function loadRunOutput(session: SessionId, run_id: string): Promise<void> {
  return reconcileRun(session, run_id);
}

export async function sendChat(session: SessionId, conversation: ConversationId, text: string, attachments: Attachment[]): Promise<void> {
  const chat = useChat.getState();
  // Optimistic: the message shows at once and is taken back if the send fails.
  const item = chat.addUser(conversation, text, attachments);
  chat.clearAttachments(conversation);
  chat.setDraft(conversation, "");
  try {
    await backend().invoke("chat_send", { session, conversation, text, ...(attachments.length ? { attachments } : {}) });
  } catch (e) {
    const now = useChat.getState();
    now.removeItem(conversation, item);
    if ((now.drafts[conversation] ?? "") === "") now.setDraft(conversation, text);
    if ((now.pendingAttachments[conversation] ?? []).length === 0) for (const a of attachments) now.addAttachment(conversation, a);
    notifyError(e);
    // Whether the conversation is busy is the backend's call (the send may have
    // been refused because a turn is running).
    try {
      await reconcileSessionInfo(session);
    } catch {
      now.setBusy(conversation, false);
    }
  }
}

export async function cancelChat(session: SessionId, conversation: ConversationId): Promise<void> {
  try {
    await backend().invoke("chat_cancel", { session, conversation });
  } catch (e) {
    notifyError(e);
  }
}

/**
 * Answers a proposal. Guards against double submission (a second call while
 * one is in flight, or for a proposal that is no longer pending, does nothing)
 * and resolves true only when the backend accepted the decision.
 */
export async function respondProposal(session: SessionId, proposal_id: ProposalId, decision: ProposalDecision): Promise<boolean> {
  const chat = useChat.getState();
  const rec = chat.proposals[proposal_id];
  if (chat.responding[proposal_id] || (rec && rec.status !== "pending")) return false;
  chat.setResponding(proposal_id, true);
  try {
    await backend().invoke("proposal_respond", { session, proposal_id, decision });
    useChat.getState().resolveProposal(proposal_id, decision.decision, "user");
    return true;
  } catch (e) {
    // `not_found`: it was resolved elsewhere (cancelled turn, policy) and the event has not arrived or was dropped.
    if (isIpcError(e) && e.code === "not_found" && useChat.getState().proposals[proposal_id]?.status === "pending") {
      useChat.getState().resolveProposal(proposal_id, "expired", null);
    }
    notifyError(e);
    return false;
  } finally {
    useChat.getState().setResponding(proposal_id, false);
  }
}

export async function newConversation(session: SessionId): Promise<void> {
  try {
    const conv = await backend().invoke("conversation_new", { session });
    useChat.getState().ensure(conv.id);
    const info = await backend().invoke("session_info", { session });
    applySessionInfo(info, false);
  } catch (e) {
    notifyError(e);
  }
}

export async function selectConversation(session: SessionId, conversation: ConversationId): Promise<void> {
  try {
    await backend().invoke("conversation_select", { session, conversation });
    useChat.getState().ensure(conversation);
    const info = await backend().invoke("session_info", { session });
    applySessionInfo(info, false);
    await loadTranscript(session, conversation);
  } catch (e) {
    notifyError(e);
  }
}

export async function openTerminal(session: SessionId, cols = 80, rows = 24): Promise<void> {
  try {
    const id = await backend().invoke("terminal_open", { session, cols, rows });
    useTerminals.getState().add(session, id);
  } catch (e) {
    notifyError(e);
  }
}

export async function closeTerminal(session: SessionId, terminal_id: string): Promise<void> {
  useTerminals.getState().remove(session, terminal_id);
  dropTerminal(terminal_id);
  try {
    await backend().invoke("terminal_close", { terminal_id });
  } catch {
    // already gone
  }
}

export async function startTask(session: SessionId, label: string, fn: () => Promise<string>): Promise<void> {
  try {
    const id = await fn();
    useTasks.getState().start(session, id, label);
  } catch (e) {
    notifyError(e);
  }
}
