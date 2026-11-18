// Applies SessionEvents (IPC.md section 3 and 7) to the stores. One
// subscription per open session; events for sessions that are not active
// still update state.
import { backend } from "../ipc";
import type { Deck, SessionEvent, SessionId } from "../ipc";
import { flushAgentText, queueAgentText, useChat } from "./chat";
import { useDiagnostics } from "./diagnostics";
import { applyDeckEvent } from "./deckEvents";
import { useDecks } from "./decks";
import { noteEvent } from "./eventSeq";
import { useFiles } from "./files";
import { checkExternalChanges } from "./fileWatch";
import { applySessionInfo, reconcileRun, refreshIndex, reloadDirsSoon, resyncSession } from "./loaders";
import { packagesChanged } from "./packages";
import { flushRunOutput, queueRunOutput, useRuns } from "./runs";
import { useSessions } from "./sessions";
import { useTasks } from "./tasks";
import { deliverTerminalData, dropTerminal, useTerminals } from "./terminals";
import { useUi } from "./ui";

/** Background refetches must never surface as unhandled rejections. */
function quietly(p: Promise<unknown>): void {
  p.catch(() => undefined);
}

export function applyEvent(session: SessionId, e: SessionEvent): void {
  noteEvent(session, e.type);
  // Text deltas are applied per frame. Anything else that can touch a
  // conversation applies the queued text first, so items keep their order.
  if (e.type !== "agent_text" && e.type !== "run_output" && e.type !== "terminal_data" && e.type !== "task_log") flushAgentText();
  switch (e.type) {
    case "agent_text":
      queueAgentText(e.conversation, e.turn, e.delta);
      break;
    case "agent_tool_call":
      useChat.getState().toolCall(e.conversation, e.turn, e.id, e.name, e.arguments);
      break;
    case "agent_tool_result":
      useChat.getState().toolResult(e.conversation, e.id, e.ok, e.summary);
      break;
    case "proposal":
      useChat.getState().addProposal(session, e.proposal);
      break;
    case "proposal_resolved":
      useChat.getState().resolveProposal(e.proposal_id, e.decision, e.by);
      break;
    case "turn_started":
      useChat.getState().turnStarted(e.conversation, e.turn);
      break;
    case "turn_done":
      useChat.getState().turnDone(e.conversation, e.turn, e.summary);
      break;
    case "turn_error":
      useChat.getState().turnError(e.conversation, e.turn, e.message);
      break;
    case "usage":
      useChat.getState().setUsage(e.conversation, {
        input_tokens: e.input_tokens,
        output_tokens: e.output_tokens,
        context_window: e.context_window,
        context_used: e.context_used,
      });
      break;
    case "run_started":
      useRuns.getState().upsertRun(session, e.run);
      // The run names the conversation whose turn started it; manual and
      // terminal runs (null) get no chat card.
      if (e.run.conversation) useChat.getState().addRun(e.run.conversation, e.run.meta.run_id);
      break;
    case "run_output":
      queueRunOutput(e.run_id, e.stream, e.chunk);
      break;
    case "run_artifact":
      useRuns.getState().addArtifact(session, e.run_id, e.artifact, e.figure);
      break;
    case "run_done":
      flushRunOutput();
      useRuns.getState().upsertRun(session, e.run);
      if (e.run.conversation) useChat.getState().addRun(e.run.conversation, e.run.meta.run_id);
      // Output chunks may have been coalesced or dropped on the way; the
      // finished run is read once more so status, artifacts and output agree
      // with what the backend recorded.
      quietly(reconcileRun(session, e.run.meta.run_id));
      break;
    case "diagnostics":
      useDiagnostics.getState().replace(session, e.paths, e.diagnostics);
      break;
    case "files_changed": {
      reloadDirsSoon(session, useFiles.getState().invalidateDirs(session, e.paths));
      void checkExternalChanges(session, e.paths);
      // The backend re-indexes on its own and announces the result with
      // `index_changed`; that event is the one fetch. Until then the index
      // shown is only marked as possibly out of date.
      if (e.paths.some((p) => p.endsWith(".py"))) useFiles.getState().markIndexStale(session);
      break;
    }
    case "index_changed":
      useFiles.getState().markIndexStale(session);
      quietly(refreshIndex(session));
      break;
    case "terminal_data":
      deliverTerminalData(e.terminal_id, e.data);
      break;
    case "terminal_exit": {
      const known = Object.values(useTerminals.getState().terminals).some((list) => list.some((t) => t.id === e.terminal_id));
      // A terminal whose tab is still open keeps its screen so the scrollback
      // stays readable; one that was already closed is released.
      if (known) useTerminals.getState().exited(e.terminal_id, e.code);
      else dropTerminal(e.terminal_id);
      break;
    }
    case "kernel_state":
      useSessions.getState().setKernel(session, e.state, e.message);
      if (e.state === "error" && e.message) useUi.getState().toast("error", `Kernel error: ${e.message}`);
      // The index needs the kernel: fetch it as soon as the kernel is up.
      if (e.state === "ready") quietly(refreshIndex(session));
      break;
    case "packages_changed":
      packagesChanged(session);
      break;
    case "task_log":
      useTasks.getState().log(e.task_id, e.line);
      break;
    case "task_done":
      useTasks.getState().done(e.task_id, e.ok, e.message);
      break;
    case "session_changed":
      applySessionInfo(e.info, false);
      break;
    case "resync":
      quietly(resyncSession(session));
      break;
    case "deck":
      applyDeckEvent(session, e.deck, e.event as Deck.DeckEvent);
      break;
    case "decks_changed":
      useDecks.getState().setList(session, e.decks);
      break;
  }
}

interface Subscription {
  ready: Promise<void>;
  unsubscribe: (() => void) | null;
  cancelled: boolean;
}

const subscriptions = new Map<SessionId, Subscription>();

/**
 * Opens the event channel of an open session (call `session_open` first:
 * subscribing to a session that is not open is `not_found`). Concurrent calls
 * share one subscription; a failed attempt leaves nothing behind, so the next
 * call tries again.
 */
export function subscribeSession(session: SessionId): Promise<void> {
  const existing = subscriptions.get(session);
  if (existing) return existing.ready;
  const sub: Subscription = { ready: Promise.resolve(), unsubscribe: null, cancelled: false };
  sub.ready = backend()
    .subscribe(session, (e) => {
      if (!sub.cancelled) applyEvent(session, e);
    })
    .then(
      (unsubscribe) => {
        if (sub.cancelled) callUnsubscribe(unsubscribe);
        else sub.unsubscribe = unsubscribe;
      },
      (e: unknown) => {
        if (subscriptions.get(session) === sub) subscriptions.delete(session);
        throw e;
      },
    );
  subscriptions.set(session, sub);
  return sub.ready;
}

function callUnsubscribe(unsubscribe: () => void): void {
  try {
    unsubscribe();
  } catch {
    // The backend drops subscriptions when a session closes; a redundant unsubscribe is harmless.
  }
}

export function unsubscribeSession(session: SessionId): void {
  const sub = subscriptions.get(session);
  if (!sub) return;
  subscriptions.delete(session);
  sub.cancelled = true;
  if (sub.unsubscribe) callUnsubscribe(sub.unsubscribe);
}

export function isSubscribed(session: SessionId): boolean {
  return subscriptions.has(session);
}

/** Drops every subscription (tests, and a full renderer reset). */
export function unsubscribeAll(): void {
  for (const session of [...subscriptions.keys()]) unsubscribeSession(session);
}
