// The environment's package list, cached per session. Listing packages spawns
// `uv` in the backend (close to a second), so the list is fetched once and
// then only again after `packages_changed` or an explicit refresh; opening the
// panel shows the cached list at once.
import { create } from "zustand";
import { backend } from "../ipc";
import type { PackageInfo, SessionId } from "../ipc";

interface PackagesState {
  lists: Record<SessionId, PackageInfo[]>;
  /** The cached list no longer matches the environment; the next `ensurePackages` refetches. */
  stale: Record<SessionId, boolean>;
  loading: Record<SessionId, boolean>;
  markStale(session: SessionId): void;
  dropSession(session: SessionId): void;
}

function without<T>(rec: Record<SessionId, T>, session: SessionId): Record<SessionId, T> {
  const next = { ...rec };
  delete next[session];
  return next;
}

export const usePackages = create<PackagesState>((set) => ({
  lists: {},
  stale: {},
  loading: {},
  markStale: (session) => set((s) => (s.stale[session] ? s : { stale: { ...s.stale, [session]: true } })),
  dropSession: (session) => set((s) => ({ lists: without(s.lists, session), stale: without(s.stale, session), loading: without(s.loading, session) })),
}));

const inFlight = new Map<SessionId, { promise: Promise<void>; again: boolean }>();

/** Fetches the list now. Concurrent calls share one request; one that arrives mid-flight causes a single follow-up. */
export function refreshPackages(session: SessionId): Promise<void> {
  const running = inFlight.get(session);
  if (running) {
    running.again = true;
    return running.promise;
  }
  const entry = { promise: Promise.resolve(), again: false };
  usePackages.setState((s) => ({ loading: { ...s.loading, [session]: true } }));
  entry.promise = (async () => {
    try {
      for (;;) {
        entry.again = false;
        const list = await backend().invoke("env_packages", { session });
        if (!entry.again) {
          usePackages.setState((s) => ({ lists: { ...s.lists, [session]: list }, stale: { ...s.stale, [session]: false } }));
          return;
        }
      }
    } finally {
      inFlight.delete(session);
      usePackages.setState((s) => ({ loading: { ...s.loading, [session]: false } }));
    }
  })();
  inFlight.set(session, entry);
  return entry.promise;
}

/** Fetches the list only if there is none yet or it was marked stale. */
export function ensurePackages(session: SessionId): Promise<void> {
  const s = usePackages.getState();
  if (s.lists[session] !== undefined && !s.stale[session]) return Promise.resolve();
  // A request already on its way answers this one too; it is not a reason to ask twice.
  return inFlight.get(session)?.promise ?? refreshPackages(session);
}

/** `packages_changed`: the cached list is out of date, and so is the answer of a request in flight. */
export function packagesChanged(session: SessionId): void {
  const running = inFlight.get(session);
  if (running) running.again = true;
  usePackages.getState().markStale(session);
}

export function usePackageList(session: SessionId | null): PackageInfo[] | null {
  return usePackages((s) => (session ? (s.lists[session] ?? null) : null));
}
