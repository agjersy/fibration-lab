// URLs for the files of a deck the webview shows: image assets, slide thumbnails (preview and
// fidelity renders), layout thumbnails and source page images. The backend hands out paths
// relative to the deck's folder (`decks/<slug>/` in the session's repository) or absolute paths;
// inside Tauri they load through the asset protocol (the shell allows each open session's deck
// cache, assets and references folders in the asset scope, IPC.md section 9). The mock registers
// in-memory URLs for the files it invents. Base64 data URLs pass through unchanged.
import { convertFileSrc } from "@tauri-apps/api/core";
import { hasTauri } from "../ipc/backend";

const registered = new Map<string, string>();

function key(session: string, deck: string, path: string): string {
  return `${session}\n${deck}\n${path}`;
}

/** The mock backend's way of making a file it invents loadable. */
export function registerDeckFile(session: string, deck: string, path: string, url: string): void {
  registered.set(key(session, deck, path), url);
}

/** The absolute folder of a deck: `<repo>/decks/<slug>`. */
export function deckRoot(repoAbs: string, deck: string): string {
  return `${repoAbs.replace(/[\\/]+$/, "")}/decks/${deck}`;
}

/**
 * A loadable URL for a path of a deck, or null when there is none in this environment (browser
 * mode without a registered file).
 */
export function deckFileUrl(session: string, repoAbs: string | null | undefined, deck: string, path: string | null | undefined): string | null {
  if (!path) return null;
  if (/^(data|blob|https?|asset):/.test(path)) return path;
  const known = registered.get(key(session, deck, path));
  if (known) return known;
  if (!hasTauri()) return null;
  if (path.startsWith("/") || /^[A-Za-z]:[\\/]/.test(path)) return convertFileSrc(path);
  if (!repoAbs) return null;
  return convertFileSrc(`${deckRoot(repoAbs, deck)}/${path.replace(/^\.\//, "")}`);
}
