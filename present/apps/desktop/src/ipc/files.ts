// URLs for project files the webview shows (image assets, thumbnails, page images). IPC.md hands out
// project-relative paths; inside Tauri they resolve through the asset protocol against the project
// directory, and the mock registers in-memory URLs for the files it invents.
import { hasTauri } from "@wb/ui";

const registered = new Map<string, string>();

export function registerProjectFile(project: string, path: string, url: string): void {
  registered.set(`${project}\n${path}`, url);
}

/** A loadable URL for a project-relative path, or null when there is none in this environment. */
export function projectFileUrl(project: { id: string; path_abs: string }, path: string | null | undefined): string | null {
  if (!path) return null;
  if (/^(data|blob|https?|asset):/.test(path) || path.startsWith("/")) return path;
  const known = registered.get(`${project.id}\n${path}`);
  if (known) return known;
  if (hasTauri()) {
    const internals = (window as { __TAURI_INTERNALS__?: { convertFileSrc?: (p: string, protocol?: string) => string } }).__TAURI_INTERNALS__;
    const abs = `${project.path_abs.replace(/[\\/]$/, "")}/${path}`;
    return internals?.convertFileSrc ? internals.convertFileSrc(abs, "asset") : null;
  }
  return null;
}
