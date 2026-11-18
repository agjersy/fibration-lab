// The verification queue of each open session (IPC.md section 10): read with
// `verification_list`, changed with `verification_set`, reloaded on `verification_changed` and
// on anything that can change an item's content (files, runs, slides).
import { useEffect } from "react";
import { create } from "zustand";
import { backend } from "../ipc";
import type { SessionId, VerificationState, VerificationView, VerifyItem, VerifyState } from "../ipc";
import { notifyError } from "./ui";

interface VerificationStore {
  lists: Record<SessionId, VerificationView[]>;
  set(session: SessionId, list: VerificationView[]): void;
}

export const useVerification = create<VerificationStore>((set) => ({
  lists: {},
  set: (session, list) => set((s) => ({ lists: { ...s.lists, [session]: list } })),
}));

const EMPTY: VerificationView[] = [];

/** The session's queue; the first user of a session reads it. */
export function useVerificationList(session: SessionId | null): VerificationView[] {
  const known = useVerification((s) => (session ? s.lists[session] !== undefined : true));
  useEffect(() => {
    if (session && !known) reloadVerificationSoon(session);
  }, [session, known]);
  return useVerification((s) => (session ? (s.lists[session] ?? EMPTY) : EMPTY));
}

/** The item as one string, as the backend keys it. */
export function itemKey(item: VerifyItem): string {
  switch (item.kind) {
    case "equation":
      return `equation:${item.path}#${item.qualname}`;
    case "figure":
      return `figure:${item.run_id}/${item.artifact}`;
    case "result":
      return `result:${item.run_id}/${item.name}`;
    case "slide_claim":
      return `slide_claim:${item.deck}/${item.slide_id}/${item.element_id}`;
    case "code_hunk":
      return `code_hunk:${item.proposal_id}`;
  }
}

/** The state of an item by key (`unverified` when the queue does not know it). */
export function stateOf(list: VerificationView[], item: VerifyItem): VerificationState {
  const key = itemKey(item);
  return list.find((v) => v.key === key)?.state ?? "unverified";
}

/** Slide claims of one slide by element id. */
export function slideClaimStates(list: VerificationView[], deck: string, slideId: string): Map<string, VerificationState> {
  const out = new Map<string, VerificationState>();
  for (const v of list) if (v.item.kind === "slide_claim" && v.item.deck === deck && v.item.slide_id === slideId) out.set(v.item.element_id, v.state);
  return out;
}

export async function loadVerification(session: SessionId): Promise<void> {
  try {
    useVerification.getState().set(session, await backend().invoke("verification_list", { session }));
  } catch {
    // The queue is advisory; a failed read leaves the last one.
  }
}

const timers = new Map<SessionId, ReturnType<typeof setTimeout>>();

/** Reloads the queue shortly (events come in bursts). */
export function reloadVerificationSoon(session: SessionId): void {
  const t = timers.get(session);
  if (t) clearTimeout(t);
  timers.set(
    session,
    setTimeout(() => {
      timers.delete(session);
      void loadVerification(session);
    }, 150),
  );
}

export async function setVerification(session: SessionId, item: VerifyItem, state: VerifyState, note?: string): Promise<boolean> {
  try {
    await backend().invoke("verification_set", { session, item, state, ...(note ? { note } : {}) });
    await loadVerification(session);
    return true;
  } catch (e) {
    notifyError(e);
    return false;
  }
}

export const STATE_LABEL: Record<VerificationState, string> = {
  unverified: "unverified",
  machine_checked: "machine-checked",
  human_verified: "verified",
  flagged: "flagged",
};

export const STATE_TONE: Record<VerificationState, string> = {
  unverified: "warning",
  machine_checked: "accent",
  human_verified: "success",
  flagged: "error",
};
