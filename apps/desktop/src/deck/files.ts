// URLs for the files of a deck the webview shows: image assets, slide thumbnails (preview and
// fidelity renders), layout thumbnails and source page images. The backend hands out paths
// relative to the deck's folder (`decks/<slug>/` in the session's repository) or absolute paths;
// inside Tauri they load through the asset protocol (the shell allows each open session's deck
// cache, assets and references folders in the asset scope, IPC.md section 9). In a browser on
// the development bridge they are fetched once with `fs_read_binary` and shown as data URLs;
// until one arrives the path resolves to null and `useDeckFilesVersion` changes when it does.
// Base64 data URLs pass through unchanged.
import { convertFileSrc } from "@tauri-apps/api/core";
import { useSyncExternalStore } from "react";
import { backend, hasTauri } from "../ipc/backend";

const fetched = new Map<string, string>();
const pending = new Set<string>();
const listeners = new Set<() => void>();
let version = 0;

function key(session: string, deck: string, path: string): string {
  return `${session}\n${deck}\n${path}`;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Changes whenever a deck file fetched over the bridge becomes available. */
export function useDeckFilesVersion(): number {
  return useSyncExternalStore(subscribe, () => version);
}

/** The absolute folder of a deck: `<repo>/decks/<slug>`. */
export function deckRoot(repoAbs: string, deck: string): string {
  return `${repoAbs.replace(/[\\/]+$/, "")}/decks/${deck}`;
}

/** Browser mode: read the file through the bridge (a path relative to the repository). */
function fetchOverBridge(session: string, repoAbs: string | null | undefined, deck: string, path: string): void {
  const k = key(session, deck, path);
  if (pending.has(k)) return;
  let rel: string;
  if (path.startsWith("/")) {
    const root = repoAbs?.replace(/[\\/]+$/, "");
    if (!root || !path.startsWith(`${root}/`)) return;
    rel = path.slice(root.length + 1);
  } else {
    rel = `decks/${deck}/${path.replace(/^\.\//, "")}`;
  }
  pending.add(k);
  let b;
  try {
    b = backend();
  } catch {
    return;
  }
  b.invoke("fs_read_binary", { session, path: rel })
    .then((d) => {
      fetched.set(k, `data:${d.mime};base64,${d.base64}`);
      version += 1;
      for (const l of listeners) l();
    })
    .catch(() => undefined);
}

/**
 * A loadable URL for a path of a deck, or null when there is none yet (browser mode, before the
 * bridge has returned the file).
 */
export function deckFileUrl(session: string, repoAbs: string | null | undefined, deck: string, path: string | null | undefined): string | null {
  if (!path) return null;
  if (/^(data|blob|https?|asset):/.test(path)) return path;
  if (!hasTauri()) {
    const known = fetched.get(key(session, deck, path));
    if (known) return known;
    fetchOverBridge(session, repoAbs, deck, path);
    return null;
  }
  if (path.startsWith("/") || /^[A-Za-z]:[\\/]/.test(path)) return convertFileSrc(path);
  if (!repoAbs) return null;
  return convertFileSrc(`${deckRoot(repoAbs, deck)}/${path.replace(/^\.\//, "")}`);
}
