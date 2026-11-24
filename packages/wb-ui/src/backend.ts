// Ported from Fibration Lab apps/desktop/src/ipc/backend.ts at commit f229e4d. Made generic over the
// command map, the event type and the subscription scope so both workbench apps share the shape.

/** One entry of a command map: `{ args, result }`. */
export interface CommandSpec {
  args: unknown;
  result: unknown;
}

/** A command map: an interface whose members are `CommandSpec`s keyed by the command name. */
export type CommandMap<C> = { [K in keyof C]: CommandSpec };

/**
 * The boundary between the renderer and the core. `invoke` runs a named command; `subscribe` opens the
 * ordered event channel of one scope (a session in Analyst, a project in Present) and returns the
 * function that closes it.
 */
export interface Backend<C extends CommandMap<C>, E, Scope = string> {
  invoke<K extends keyof C>(cmd: K, args: C[K]["args"]): Promise<C[K]["result"]>;
  subscribe(scope: Scope, onEvent: (e: E) => void): Promise<() => void>;
}

/** The structured error every command may reject with. */
export interface IpcError<Code extends string = string> {
  code: Code;
  message: string;
  detail?: unknown;
}

export function isIpcError(e: unknown): e is IpcError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e && typeof (e as IpcError).message === "string";
}

export function errorMessage(e: unknown): string {
  if (isIpcError(e)) return `${e.message} (${e.code})`;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function ipcError<Code extends string>(code: Code, message: string, detail?: unknown): IpcError<Code> {
  return detail === undefined ? { code, message } : { code, message, detail };
}

export function hasTauri(): boolean {
  return typeof window !== "undefined" && (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== undefined;
}

export interface BackendHolder<B> {
  /** Picks the injected backend, else the Tauri one inside the desktop shell, else the browser one (the development bridge). */
  getBackend(): Promise<B>;
  /** Synchronous accessor for code paths that run after bootstrap. */
  backend(): B;
  setBackend(b: B | null): void;
}

/**
 * Lazily selects a backend. `globalKey` names a `window` property that tests and the smoke harness may
 * set to inject one; the chosen instance is stored there too.
 */
export function createBackendHolder<B>(options: { globalKey: string; loadTauri: () => Promise<B>; loadBrowser: () => Promise<B> }): BackendHolder<B> {
  let instance: B | null = null;
  const globals = (): Record<string, unknown> => (typeof window === "undefined" ? {} : (window as unknown as Record<string, unknown>));
  return {
    async getBackend() {
      if (instance) return instance;
      const injected = globals()[options.globalKey] as B | undefined;
      if (injected) {
        instance = injected;
        return instance;
      }
      instance = hasTauri() ? await options.loadTauri() : await options.loadBrowser();
      globals()[options.globalKey] = instance;
      return instance;
    },
    backend() {
      if (!instance) throw new Error("backend not initialised; call getBackend() first");
      return instance;
    },
    setBackend(b) {
      instance = b;
    },
  };
}
