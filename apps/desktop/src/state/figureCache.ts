// Figure bytes as data: URLs, keyed by session and repo-relative path. The
// cache is a small LRU: figures can be megabytes each and a long session
// produces hundreds, so only the most recently used ones are kept. Components
// hold on to the URL they are showing, so eviction never blanks a visible figure.
import { useEffect, useState } from "react";
import { backend } from "../ipc";
import type { SessionId } from "../ipc";

export const MAX_FIGURES_CACHED = 64;

export class LruCache<V> {
  private readonly map = new Map<string, V>();
  constructor(private readonly capacity: number) {}

  get(key: string): V | undefined {
    const v = this.map.get(key);
    if (v === undefined) return undefined;
    // Re-insert to mark as most recently used.
    this.map.delete(key);
    this.map.set(key, v);
    return v;
  }

  set(key: string, value: V): void {
    this.map.delete(key);
    this.map.set(key, value);
    while (this.map.size > this.capacity) {
      const oldest = this.map.keys().next();
      if (oldest.done) break;
      this.map.delete(oldest.value);
    }
  }

  delete(key: string): void {
    this.map.delete(key);
  }

  deleteWhere(pred: (key: string) => boolean): void {
    for (const key of [...this.map.keys()]) if (pred(key)) this.map.delete(key);
  }

  get size(): number {
    return this.map.size;
  }

  keys(): string[] {
    return [...this.map.keys()];
  }
}

const cache = new LruCache<string>(MAX_FIGURES_CACHED);
const inflight = new Map<string, Promise<string>>();

const keyOf = (session: SessionId, path: string) => `${session}\n${path}`;

export function ensureFigureData(session: SessionId, path: string): Promise<string> {
  const key = keyOf(session, path);
  const cached = cache.get(key);
  if (cached !== undefined) return Promise.resolve(cached);
  const running = inflight.get(key);
  if (running) return running;
  const p = backend()
    .invoke("figure_data", { session, path })
    .then((data) => {
      const url = `data:${data.mime};base64,${data.base64}`;
      cache.set(key, url);
      return url;
    })
    .finally(() => inflight.delete(key));
  inflight.set(key, p);
  return p;
}

export function dropSessionFigures(session: SessionId): void {
  cache.deleteWhere((k) => k.startsWith(`${session}\n`));
}

export function figureCacheSize(): number {
  return cache.size;
}

/** data: URL for a figure, or null while loading. The component keeps its own reference. */
export function useFigureData(session: SessionId, path: string): string | null {
  const key = keyOf(session, path);
  const [state, setState] = useState<{ key: string; url: string } | null>(null);
  useEffect(() => {
    let live = true;
    ensureFigureData(session, path)
      .then((url) => {
        if (live) setState({ key, url });
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [session, path, key]);
  return state && state.key === key ? state.url : null;
}
