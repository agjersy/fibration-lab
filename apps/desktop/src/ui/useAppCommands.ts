// Registers the command table and the global key handler.
import { useEffect } from "react";
import { registerCommands, runCommand } from "../commands";
import { resolveKey } from "../keymap";
import type { KeyContext } from "../keymap";
import { openTerminal, refreshIndex, runCurrentScript, saveBuffer } from "../state/actions";
import { useSessions } from "../state/sessions";
import { useUi } from "../state/ui";
import { activeEditorPath, closableTabFocused, closeActiveTab, cycleTab, focusChatInput, showPanel, toggleDock } from "./workbench/controller";

export function contextOf(target: EventTarget | null): KeyContext {
  const el = target instanceof Element ? target : null;
  // Focus inside an xterm: the shell owns the keyboard (see keymap.ts).
  if (el?.closest(".xterm")) return "terminal";
  if (el?.closest(".cm-editor")) return "editor";
  if (el?.closest("[data-chat-input]")) return "chat";
  return "global";
}

/** A modal dialog (or the palette) is open: global shortcuts must not act on the workbench behind it. */
function modalOpen(): boolean {
  return document.querySelector('[role="dialog"][aria-modal="true"]') !== null;
}

export function useAppCommands(): void {
  useEffect(() => {
    const session = () => useSessions.getState().active;
    const unregister = registerCommands([
      { id: "file_finder", title: "Go to file", category: "Navigate", run: () => useUi.getState().setPalette("files") },
      { id: "command_palette", title: "Command palette", category: "Navigate", run: () => useUi.getState().setPalette("commands") },
      { id: "go_to_line", title: "Go to line", category: "Navigate", run: () => useUi.getState().setPalette("goto") },
      {
        id: "save",
        title: "Save file",
        category: "File",
        run: () => {
          const s = session();
          const path = activeEditorPath();
          if (s && path) void saveBuffer(s, path);
        },
      },
      { id: "close_tab", title: "Close tab", category: "File", run: closeActiveTab },
      { id: "next_tab", title: "Next tab", category: "Navigate", run: () => cycleTab(1) },
      { id: "prev_tab", title: "Previous tab", category: "Navigate", run: () => cycleTab(-1) },
      { id: "toggle_left_dock", title: "Toggle left dock", category: "View", run: () => toggleDock("sidebar") },
      { id: "toggle_bottom_dock", title: "Toggle bottom dock", category: "View", run: () => toggleDock("bottom") },
      { id: "toggle_right_dock", title: "Toggle right dock", category: "View", run: () => toggleDock("chat") },
      { id: "toggle_panels_dock", title: "Toggle figures dock", category: "View", run: () => toggleDock("panels") },
      { id: "focus_chat", title: "Focus agent chat", category: "Agent", run: focusChatInput },
      {
        id: "chat_send",
        title: "Send chat message",
        category: "Agent",
        hidden: true,
        run: () => {
          const el = document.querySelector<HTMLTextAreaElement>("textarea[data-chat-input]");
          el?.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true }));
        },
      },
      {
        id: "new_terminal",
        title: "New terminal",
        category: "Terminal",
        run: () => {
          const s = session();
          if (s) {
            showPanel("terminal");
            void openTerminal(s);
          }
        },
      },
      {
        id: "run_current_script",
        title: "Run current script",
        category: "Run",
        run: () => {
          const s = session();
          const path = activeEditorPath();
          if (!s || !path || !path.endsWith(".py")) return;
          void (async () => {
            // Saves first when dirty; a failed save aborts the run.
            const id = await runCurrentScript(s, path);
            if (id) showPanel("runoutput");
          })();
        },
      },
      {
        id: "refresh_index",
        title: "Refresh script and library index",
        category: "Run",
        run: () => {
          const s = session();
          if (s) void refreshIndex(s).catch(() => undefined);
        },
      },
      { id: "show_figures", title: "Show figures", category: "View", run: () => showPanel("figures") },
      { id: "show_equations", title: "Show equations", category: "View", run: () => showPanel("equations") },
      { id: "show_packages", title: "Show packages", category: "View", run: () => showPanel("packages") },
      { id: "show_diagnostics", title: "Show diagnostics", category: "View", run: () => showPanel("diagnostics") },
      { id: "show_run_output", title: "Show run output", category: "View", run: () => showPanel("runoutput") },
      { id: "new_session", title: "New session", category: "Session", run: () => useUi.getState().setNewSessionOpen(true) },
      { id: "settings", title: "Settings and policy", category: "Session", run: () => useUi.getState().setSettingsOpen(true) },
      { id: "theme_light", title: "Theme: light", category: "View", run: () => useUi.getState().setTheme("light") },
      { id: "theme_dark", title: "Theme: dark", category: "View", run: () => useUi.getState().setTheme("dark") },
      { id: "theme_system", title: "Theme: system", category: "View", run: () => useUi.getState().setTheme("system") },
    ]);

    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const ctx = contextOf(e.target);
      // A binding whose guard does not hold is not resolved, so the event is
      // left untouched (mod-w without a closable tab stays the platform's).
      const command = resolveKey(e, ctx, undefined, { closable_tab: closableTabFocused() });
      if (!command) return;
      // The chat input handles its own send binding.
      if (command === "chat_send") return;
      e.preventDefault();
      e.stopPropagation();
      if (modalOpen()) {
        // Only the palette's own toggles work while something modal is open.
        const palette = useUi.getState().palette;
        if (palette === null) return;
        if (command === "command_palette") useUi.getState().setPalette(palette === "commands" ? null : "commands");
        else if (command === "file_finder") useUi.getState().setPalette(palette === "files" ? null : "files");
        return;
      }
      runCommand(command);
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      unregister();
      document.removeEventListener("keydown", onKey, true);
    };
  }, []);
}
