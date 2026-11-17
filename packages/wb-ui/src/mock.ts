// Helpers for in-memory backends (the pattern of Personal Analyst apps/desktop/src/ipc/mock.ts at commit
// f229e4d: simulated latency, per-scope event fan-out, scaled streaming delays).

export interface MockTiming {
  /** [min, max] latency per command in ms; [0, 0] for tests. */
  latency?: [number, number];
  /** Scale applied to streaming delays (1 = real time, 0 = as fast as possible). */
  timeScale?: number;
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, Math.max(0, ms)));
}

export class MockClock {
  readonly latency: [number, number];
  readonly timeScale: number;
  constructor(timing: MockTiming = {}) {
    this.latency = timing.latency ?? [50, 300];
    this.timeScale = timing.timeScale ?? 1;
  }
  /** The simulated round trip of one command. */
  delay(): Promise<void> {
    const [min, max] = this.latency;
    if (max <= 0) return Promise.resolve();
    return sleep(min + Math.random() * (max - min));
  }
  /** A pause inside a streamed operation, scaled by `timeScale`. */
  wait(ms: number): Promise<void> {
    return sleep(ms * this.timeScale);
  }
}

/** Per-scope event fan-out with ordered delivery. */
export class MockEmitter<E, Scope = string> {
  private listeners = new Map<Scope, Set<(e: E) => void>>();
  subscribe(scope: Scope, fn: (e: E) => void): () => void {
    const set = this.listeners.get(scope) ?? new Set();
    set.add(fn);
    this.listeners.set(scope, set);
    return () => {
      set.delete(fn);
    };
  }
  emit(scope: Scope, event: E): void {
    for (const fn of [...(this.listeners.get(scope) ?? [])]) fn(event);
  }
  clear(scope: Scope): void {
    this.listeners.delete(scope);
  }
}
