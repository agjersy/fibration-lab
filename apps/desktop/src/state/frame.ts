// Runs a flush at most once per animation frame, however many times it is
// requested. Streamed events (run output, assistant text) are queued by their
// owners and applied through one of these, so the stores update per frame
// instead of per event.

export interface FrameBatcher {
  /** Asks for a flush on the next frame; a no-op while one is already due. */
  schedule(): void;
  /** Drops the pending frame (the caller is flushing now). */
  cancel(): void;
}

export function frameBatcher(flush: () => void): FrameBatcher {
  let scheduled: { kind: "raf"; id: number } | { kind: "timer"; id: ReturnType<typeof setTimeout> } | null = null;
  const cancel = () => {
    if (!scheduled) return;
    if (scheduled.kind === "raf") cancelAnimationFrame(scheduled.id);
    else clearTimeout(scheduled.id);
    scheduled = null;
  };
  return {
    schedule: () => {
      if (scheduled) return;
      const run = () => {
        scheduled = null;
        flush();
      };
      // Hidden documents do not get animation frames; fall back to a timer there.
      if (typeof requestAnimationFrame === "function" && typeof document !== "undefined" && document.visibilityState === "visible") {
        scheduled = { kind: "raf", id: requestAnimationFrame(run) };
      } else {
        scheduled = { kind: "timer", id: setTimeout(run, 16) };
      }
    },
    cancel,
  };
}
