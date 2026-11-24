// Guards the window against being closed with unsaved editor buffers.
//
// Browser mode: `beforeunload` makes the browser ask before leaving.
//
// Tauri: the window's close request is intercepted while (and only while) some
// buffer is dirty. Listening for it at all makes the JS side responsible for
// destroying the window, so the listener is registered when the first buffer
// becomes dirty and removed when the last one is clean; with nothing unsaved
// the window closes natively, exactly as without this guard.
import { useEffect } from "react";
import { hasTauri } from "../ipc";
import { confirmUnsaved } from "../state/actions";
import { anyDirty, dirtyPaths, useFiles } from "../state/files";

/** Saves or discards per session; false as soon as the user cancels or a save fails. */
async function settleAllUnsaved(): Promise<boolean> {
  for (const session of Object.keys(useFiles.getState().buffers)) {
    const paths = dirtyPaths(session);
    if (paths.length > 0 && !(await confirmUnsaved(session, paths, "Close window"))) return false;
  }
  return true;
}

async function watchTauriClose(): Promise<() => void> {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const win = getCurrentWindow();
  let unlisten: (() => void) | null = null;
  let registering = false;
  let stopped = false;

  const sync = async () => {
    const dirty = anyDirty();
    if (dirty && !unlisten && !registering) {
      registering = true;
      try {
        const off = await win.onCloseRequested(async (event) => {
          // Always ours to finish: ask first, then close for real.
          event.preventDefault();
          if (!(await settleAllUnsaved())) return;
          unlisten?.();
          unlisten = null;
          try {
            await win.destroy();
          } catch {
            // Without the destroy permission, a plain close now goes through natively.
            await win.close().catch(() => undefined);
          }
        });
        if (stopped || !anyDirty()) off();
        else unlisten = off;
      } catch {
        // The window API is not available (older shell, missing permission): no guard.
      } finally {
        registering = false;
      }
    } else if (!dirty && unlisten) {
      unlisten();
      unlisten = null;
    }
  };

  const unsubscribe = useFiles.subscribe(() => void sync());
  void sync();
  return () => {
    stopped = true;
    unsubscribe();
    unlisten?.();
    unlisten = null;
  };
}

export function useUnsavedGuards(): void {
  useEffect(() => {
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      if (!anyDirty()) return;
      e.preventDefault();
      // Legacy browsers need a return value to show the prompt.
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    let stopTauri: (() => void) | null = null;
    let cancelled = false;
    if (hasTauri()) {
      watchTauriClose()
        .then((stop) => {
          if (cancelled) stop();
          else stopTauri = stop;
        })
        .catch(() => undefined);
    }
    return () => {
      cancelled = true;
      window.removeEventListener("beforeunload", onBeforeUnload);
      stopTauri?.();
    };
  }, []);
}
