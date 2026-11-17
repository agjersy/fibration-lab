// Registers the command table and the global key handler. Ported from Personal Analyst
// apps/desktop/src/ui/useAppCommands.ts at commit f229e4d.
import { useEffect } from "react";
import { registerCommands, runCommand } from "@wb/ui";
import { orderedSlideIds } from "../deck/model";
import { keymap } from "../keymap";
import type { KeyContext } from "../keymap";
import { createSlide, deleteSlide, duplicateSlide, nudgeSlide, setSlideLocked, startTask } from "../state/actions";
import { useDeck } from "../state/deck";
import { useProjects } from "../state/projects";
import { useUi } from "../state/ui";
import { backend } from "../ipc";
import { cycleTab, focusChatInput, showPanel, toggleDock } from "./workbench/controller";

function contextOf(target: EventTarget | null): KeyContext {
  const el = target instanceof Element ? target : null;
  if (el?.closest("[data-slide-editor]")) return "text";
  if (el?.closest("[data-chat-input]")) return "chat";
  if (el?.closest("[data-rail]")) return "rail";
  return "global";
}

function current(): { project: string; slide: string | null } | null {
  const project = useProjects.getState().active;
  if (!project) return null;
  return { project, slide: useDeck.getState().decks[project]?.selected ?? null };
}

function stepSelection(delta: 1 | -1): void {
  const cur = current();
  const d = cur ? useDeck.getState().decks[cur.project] : undefined;
  if (!cur || !d) return;
  const ids = orderedSlideIds(d.deck);
  const next = ids[Math.min(ids.length - 1, Math.max(0, ids.indexOf(cur.slide ?? "") + delta))];
  if (next) useDeck.getState().select(cur.project, next);
}

export function useAppCommands(): void {
  useEffect(() => {
    const withSlide = (fn: (project: string, slide: string) => void) => () => {
      const cur = current();
      if (cur?.slide) fn(cur.project, cur.slide);
    };
    const unregister = registerCommands([
      { id: "command_palette", title: "Command palette", category: "Navigate", run: () => useUi.getState().setPalette("commands") },
      { id: "slide_finder", title: "Go to slide", category: "Navigate", run: () => useUi.getState().setPalette("slides") },
      { id: "next_slide", title: "Next slide", category: "Navigate", run: () => stepSelection(1) },
      { id: "prev_slide", title: "Previous slide", category: "Navigate", run: () => stepSelection(-1) },
      { id: "next_tab", title: "Next tab", category: "Navigate", run: () => cycleTab(1) },
      { id: "prev_tab", title: "Previous tab", category: "Navigate", run: () => cycleTab(-1) },
      { id: "toggle_left_dock", title: "Toggle left dock", category: "View", run: () => toggleDock("sidebar") },
      { id: "toggle_bottom_dock", title: "Toggle bottom dock", category: "View", run: () => toggleDock("bottom") },
      { id: "toggle_right_dock", title: "Toggle right dock", category: "View", run: () => toggleDock("chat") },
      { id: "focus_chat", title: "Focus agent chat", category: "Agent", run: focusChatInput },
      { id: "show_slides", title: "Show slides", category: "View", run: () => showPanel("slides") },
      { id: "show_outline", title: "Show outline", category: "View", run: () => showPanel("outline") },
      { id: "show_references", title: "Show references", category: "View", run: () => showPanel("references") },
      { id: "show_sources", title: "Show sources", category: "View", run: () => showPanel("sources") },
      { id: "show_checks", title: "Show checks", category: "View", run: () => showPanel("checks") },
      { id: "show_notes", title: "Show notes", category: "View", run: () => showPanel("notes") },
      { id: "show_exports", title: "Show exports", category: "View", run: () => showPanel("exports") },
      { id: "move_slide_down", title: "Move slide down", category: "Slide", run: withSlide((p, s) => void nudgeSlide(p, s, 1)) },
      { id: "move_slide_up", title: "Move slide up", category: "Slide", run: withSlide((p, s) => void nudgeSlide(p, s, -1)) },
      { id: "duplicate_slide", title: "Duplicate slide", category: "Slide", run: withSlide((p, s) => void duplicateSlide(p, s)) },
      { id: "delete_slide", title: "Delete slide", category: "Slide", run: withSlide((p, s) => void deleteSlide(p, s)) },
      {
        id: "toggle_slide_lock",
        title: "Lock or unlock slide",
        category: "Slide",
        run: withSlide((p, s) => void setSlideLocked(p, s, !useDeck.getState().decks[p]?.slides[s]?.slide.locked)),
      },
      {
        id: "new_slide",
        title: "New slide",
        category: "Slide",
        run: () => {
          const cur = current();
          if (cur) void createSlide(cur.project, "Title and Content", cur.slide ?? undefined);
        },
      },
      { id: "change_layout", title: "Change slide layout", category: "Slide", run: () => useUi.getState().setLayoutPickerOpen(true) },
      {
        id: "render_fidelity",
        title: "Render fidelity thumbnails",
        category: "Deck",
        run: () => {
          const cur = current();
          if (cur) void startTask(cur.project, "render thumbnails", () => backend().invoke("render_fidelity", { project: cur.project }));
        },
      },
      { id: "new_project", title: "New project", category: "Project", run: () => useUi.getState().setNewProjectOpen(true) },
      { id: "settings", title: "Settings", category: "Project", run: () => useUi.getState().setSettingsOpen(true) },
      { id: "theme_light", title: "Theme: light", category: "View", run: () => useUi.getState().setTheme("light") },
      { id: "theme_dark", title: "Theme: dark", category: "View", run: () => useUi.getState().setTheme("dark") },
      { id: "theme_system", title: "Theme: system", category: "View", run: () => useUi.getState().setTheme("system") },
    ]);

    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const ctx = contextOf(e.target);
      const command = keymap.resolveKey(e, ctx);
      if (!command) return;
      // The slide editor and the chat input handle their own bindings.
      if (command.startsWith("text_") || command === "chat_send") return;
      e.preventDefault();
      e.stopPropagation();
      if (command === "command_palette" && useUi.getState().palette === "commands") {
        useUi.getState().setPalette(null);
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
