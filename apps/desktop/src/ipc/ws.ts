import type { Backend } from "./backend";
import type { Commands } from "./commands";
import type { IpcError, SessionEvent, SessionId } from "./types";

type Pending = { resolve: (v: unknown) => void; reject: (e: unknown) => void };

/**
 * The renderer in a plain browser, talking to the real core through the development bridge
 * (`fibration-serve`, docs/IPC.md section 11) over one WebSocket. The URL carries the bridge's
 * token: `ws://127.0.0.1:<port>/?token=<token>`. Requests are `{id, cmd, args}`; replies are
 * `{id, ok}` or `{id, error}`; events arrive as `{channel, event}` for the channel number this
 * side passed to `session_subscribe`, the stand-in for a Tauri channel.
 */
export class WsBackend implements Backend {
  private socket: Promise<WebSocket>;
  private nextId = 1;
  private nextChannel = 1;
  private pending = new Map<number, Pending>();
  private channels = new Map<number, (e: SessionEvent) => void>();

  constructor(url: string) {
    this.socket = new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      ws.onopen = () => resolve(ws);
      ws.onerror = () => reject(transportError(`cannot reach the development bridge at ${redact(url)}`));
      ws.onclose = () => {
        const err = transportError("the development bridge closed the connection");
        for (const p of this.pending.values()) p.reject(err);
        this.pending.clear();
      };
      ws.onmessage = (m) => this.receive(String(m.data));
    });
  }

  private receive(data: string): void {
    let msg: { id?: number; ok?: unknown; error?: IpcError; channel?: number; event?: SessionEvent };
    try {
      msg = JSON.parse(data);
    } catch {
      return;
    }
    if (typeof msg.channel === "number" && msg.event) {
      this.channels.get(msg.channel)?.(msg.event);
      return;
    }
    if (typeof msg.id !== "number") return;
    const p = this.pending.get(msg.id);
    if (!p) return;
    this.pending.delete(msg.id);
    if (msg.error) p.reject(msg.error);
    else p.resolve(msg.ok ?? null);
  }

  private async call(cmd: string, args: unknown): Promise<unknown> {
    const ws = await this.socket;
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, cmd, args }));
    });
  }

  async invoke<K extends keyof Commands>(cmd: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]> {
    return (await this.call(cmd, args)) as Commands[K]["result"];
  }

  async subscribe(session: SessionId, onEvent: (e: SessionEvent) => void): Promise<() => void> {
    const channel = this.nextChannel++;
    this.channels.set(channel, onEvent);
    let subscription: string;
    try {
      subscription = (await this.call("session_subscribe", { session, channel })) as string;
    } catch (e) {
      this.channels.delete(channel);
      throw e;
    }
    return () => {
      this.channels.delete(channel);
      // After session_close the bridge has already dropped the subscription.
      this.call("session_unsubscribe", { subscription }).catch(() => undefined);
    };
  }
}

function transportError(message: string): IpcError {
  return { code: "io", message } as IpcError;
}

function redact(url: string): string {
  return url.replace(/token=[^&]*/, "token=…");
}

/**
 * The bridge URL for browser mode: the `serve` query parameter of the page
 * (`?serve=ws%3A%2F%2F127.0.0.1%3A<port>%2F%3Ftoken%3D<token>`), else `VITE_FIBRATION_SERVE`.
 */
export function serveUrl(): string | null {
  const fromPage = typeof location !== "undefined" ? new URLSearchParams(location.search).get("serve") : null;
  const fromEnv = (import.meta.env.VITE_FIBRATION_SERVE as string | undefined) ?? null;
  const url = fromPage || fromEnv;
  return url && /^ws:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?(\/|\?|$)/.test(url) ? url : null;
}
