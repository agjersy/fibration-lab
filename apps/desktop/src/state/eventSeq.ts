// Counts the state-bearing events seen per session. A `session_info` answer
// is a snapshot taken at some point during its round trip; if such an event
// arrived meanwhile, the snapshot may be older than what the events already
// said (a kernel that became ready, a turn that finished), so the caller asks
// again instead of overwriting newer state with it.
import type { SessionEvent, SessionId } from "../ipc";

const seq = new Map<SessionId, number>();

const STATE_EVENTS: ReadonlySet<SessionEvent["type"]> = new Set([
  "kernel_state",
  "turn_started",
  "turn_done",
  "turn_error",
  "proposal",
  "proposal_resolved",
  "session_changed",
]);

export function noteEvent(session: SessionId, type: SessionEvent["type"]): void {
  if (STATE_EVENTS.has(type)) seq.set(session, (seq.get(session) ?? 0) + 1);
}

export function stateEventSeq(session: SessionId): number {
  return seq.get(session) ?? 0;
}
