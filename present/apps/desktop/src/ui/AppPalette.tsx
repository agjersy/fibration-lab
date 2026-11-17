// The app's palettes on the shared Palette of @wb/ui: commands, and "go to slide".
import { useCallback } from "react";
import { Palette, commandItems, rankItems } from "@wb/ui";
import type { PaletteItem } from "@wb/ui";
import { slideTitle } from "../deck/model";
import { keymap } from "../keymap";
import { orderedViews, useDeck } from "../state/deck";
import { useActiveProject } from "../state/projects";
import { useUi } from "../state/ui";

export function AppPalette() {
  const mode = useUi((s) => s.palette);
  const project = useActiveProject();
  const getItems = useCallback(
    (query: string): PaletteItem[] => {
      if (mode === "commands") return commandItems(query, (id) => keymap.shortcut(id));
      const d = project ? useDeck.getState().decks[project] : undefined;
      if (!d || !project) return [];
      return rankItems(
        query,
        orderedViews(d).map((v) => ({ id: v.slide.id, label: `${v.index + 1}  ${slideTitle(v.slide) || "(untitled)"}`, detail: v.slide.layout, run: () => useDeck.getState().select(project, v.slide.id) })),
        (it) => it.label,
      );
    },
    [mode, project],
  );
  if (!mode) return null;
  return <Palette key={mode} label={mode === "commands" ? "Command palette" : "Go to slide"} placeholder={mode === "commands" ? "Run a command…" : "Go to slide…"} getItems={getItems} onClose={() => useUi.getState().setPalette(null)} />;
}
