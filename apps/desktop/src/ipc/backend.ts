import type { Commands } from "./commands";
import type { IpcError, SessionEvent, SessionId } from "./types";

export interface Backend {
  invoke<K extends keyof Commands>(cmd: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]>;
  subscribe(session: SessionId, onEvent: (e: SessionEvent) => void): Promise<() => void>;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
    __FIBRATION_BACKEND__?: Backend;
  }
}

export function isIpcError(e: unknown): e is IpcError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e && typeof (e as IpcError).message === "string";
}

export function errorMessage(e: unknown): string {
  if (isIpcError(e)) return `${e.message} (${e.code})`;
  if (e instanceof Error) return e.message;
  return String(e);
}

let instance: Backend | null = null;

export function hasTauri(): boolean {
  return typeof window !== "undefined" && window.__TAURI_INTERNALS__ !== undefined;
}

/**
 * Picks TauriBackend inside the desktop shell. In a plain browser the renderer talks to the real
 * core through the development bridge (`fibration-serve`), named by `?serve=` or
 * `VITE_FIBRATION_SERVE` (docs/DEVELOPMENT.md).
 */
export async function getBackend(): Promise<Backend> {
  if (instance) return instance;
  if (window.__FIBRATION_BACKEND__) {
    instance = window.__FIBRATION_BACKEND__;
    return instance;
  }
  if (hasTauri()) {
    const { TauriBackend } = await import("./tauri");
    instance = new TauriBackend();
  } else {
    const { WsBackend, serveUrl } = await import("./ws");
    const url = serveUrl();
    if (!url) {
      throw new Error("No backend: open this page inside the desktop app, or start fibration-serve and pass its URL as ?serve=ws://127.0.0.1:<port>/?token=<token> (or VITE_FIBRATION_SERVE).");
    }
    instance = new WsBackend(url);
  }
  window.__FIBRATION_BACKEND__ = instance;
  return instance;
}

/** Synchronous accessor for code paths that run after bootstrap. */
export function backend(): Backend {
  if (!instance) throw new Error("backend not initialised; call getBackend() first");
  return instance;
}

export function setBackend(b: Backend): void {
  instance = b;
}
