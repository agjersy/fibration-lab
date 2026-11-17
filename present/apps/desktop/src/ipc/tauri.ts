import { Channel, convertFileSrc, invoke } from "@tauri-apps/api/core";
import type { Backend } from "./backend";
import type { Commands } from "./commands";
import type { ProjectEvent, ProjectId } from "./types";

/**
 * Bridge to the Rust side. Argument objects are passed through untouched, so
 * the snake_case keys documented in IPC.md section 4 reach the commands as is.
 */
export class TauriBackend implements Backend {
  async invoke<K extends keyof Commands>(cmd: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]> {
    return (await invoke(cmd, args as Record<string, any>)) as Commands[K]["result"];
  }

  async subscribe(project: ProjectId, onEvent: (e: ProjectEvent) => void): Promise<() => void> {
    const channel = new Channel<ProjectEvent>();
    channel.onmessage = onEvent;
    await invoke("project_subscribe", { project, channel });
    // IPC.md has no unsubscribe command: the core drops the channel on `project_close`. Until then the
    // renderer stops listening.
    return () => {
      channel.onmessage = () => undefined;
    };
  }
}

/** URL the webview can load for a file under a project directory (thumbnails, page images, assets). */
export function tauriFileUrl(path_abs: string): string {
  return convertFileSrc(path_abs);
}
