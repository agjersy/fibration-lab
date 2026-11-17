// The Present instance of the shared Backend interface (IPC.md section 1): commands of section 4,
// events of section 3, one channel per open project.
import { createBackendHolder } from "@wb/ui";
import type { Backend as WbBackend } from "@wb/ui";
import type { Commands } from "./commands";
import type { ProjectEvent, ProjectId } from "./types";

export type Backend = WbBackend<Commands, ProjectEvent, ProjectId>;

const holder = createBackendHolder<Backend>({
  globalKey: "__PRESENT_BACKEND__",
  loadTauri: async () => new (await import("./tauri")).TauriBackend(),
  loadMock: async () => new (await import("./mock")).MockBackend(),
});

/** Picks TauriBackend inside the desktop shell, MockBackend in a plain browser. */
export const getBackend = holder.getBackend;
/** Synchronous accessor for code paths that run after bootstrap. */
export const backend = holder.backend;
export const setBackend = holder.setBackend;
