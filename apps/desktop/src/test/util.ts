// Helpers shared by the vitest suites. Not part of the app bundle.
import type { Backend, Commands, SessionEvent, SessionId } from "../ipc";
import { MockBackend } from "../ipc/mock";
import { useChat } from "../state/chat";
import { useDiagnostics } from "../state/diagnostics";
import { unsubscribeAll } from "../state/dispatcher";
import { useFiles } from "../state/files";
import { flushRunOutput, useRuns } from "../state/runs";
import { useSessions } from "../state/sessions";
import { useTasks } from "../state/tasks";
import { useTerminals } from "../state/terminals";
import { useUi } from "../state/ui";

export function waitFor<T>(check: () => T | undefined | null | false, timeoutMs = 4000, what = "condition"): Promise<T> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const tick = () => {
      let v: T | undefined | null | false;
      try {
        v = check();
      } catch (e) {
        reject(e instanceof Error ? e : new Error(String(e)));
        return;
      }
      if (v !== undefined && v !== null && v !== false) resolve(v);
      else if (Date.now() - start > timeoutMs) reject(new Error(`timeout waiting for ${what}`));
      else setTimeout(tick, 2);
    };
    tick();
  });
}

export function resetStores(): void {
  unsubscribeAll();
  flushRunOutput();
  useChat.setState({ conversations: {}, proposals: {}, drafts: {}, pendingAttachments: {}, responding: {} });
  useRuns.setState({ runs: {}, output: {}, outputOrder: [], figures: {}, selectedRun: {}, activeFigure: {}, pinned: {}, compare: {} });
  useDiagnostics.setState({ byPath: {} });
  useFiles.setState({ trees: {}, expanded: {}, buffers: {}, index: {}, indexStale: {}, activeFile: {}, reveal: {} });
  useSessions.setState({ app: null, sessions: [], infos: {}, kernel: {}, active: null, models: [], targets: [] });
  useTasks.setState({ tasks: {} });
  useTerminals.setState({ terminals: {}, active: {}, counter: {} });
  useUi.setState({ toasts: [], unsavedPrompt: null, palette: null });
}

/** The strict mock at test speed: no latency, the kernel ready `kernelMs` after open. */
export function strictMock(kernelMs = 60, seedSession = true): MockBackend {
  return new MockBackend({ latency: [0, 0], timeScale: kernelMs / 300, seedSession });
}

type Invoke = <K extends keyof Commands>(cmd: K, args: Commands[K]["args"]) => Promise<Commands[K]["result"]>;

export interface Probe extends Backend {
  /** Commands in the order they were invoked. */
  calls: (keyof Commands | "subscribe")[];
  /** While true, events from the inner backend are dropped (a lagging subscriber). */
  dropEvents: boolean;
  /** Replace to intercept a command; call `inner` to pass it on. */
  intercept: (cmd: keyof Commands, args: unknown, inner: () => Promise<unknown>) => Promise<unknown>;
  events: SessionEvent[];
}

/** Wraps a backend to record calls, intercept commands and drop events. */
export function probe(inner: Backend): Probe {
  const p: Probe = {
    calls: [],
    dropEvents: false,
    events: [],
    intercept: (_cmd, _args, next) => next(),
    invoke: (async (cmd, args) => {
      p.calls.push(cmd);
      return (await p.intercept(cmd, args, () => inner.invoke(cmd, args))) as never;
    }) as Invoke,
    subscribe: (session: SessionId, onEvent: (e: SessionEvent) => void) => {
      p.calls.push("subscribe");
      return inner.subscribe(session, (e) => {
        if (p.dropEvents) return;
        p.events.push(e);
        onEvent(e);
      });
    },
  };
  return p;
}
