import { create } from "zustand";
import { readThemePreference, writeThemePreference } from "@wb/ui";
import type { ThemePreference } from "@wb/ui";

export type PaletteMode = "commands" | "slides" | null;

const THEME_KEY = "present.theme";

interface UiState {
  theme: ThemePreference;
  palette: PaletteMode;
  newProjectOpen: boolean;
  settingsOpen: boolean;
  layoutPickerOpen: boolean;
  setTheme(theme: ThemePreference): void;
  setPalette(mode: PaletteMode): void;
  setNewProjectOpen(open: boolean): void;
  setSettingsOpen(open: boolean): void;
  setLayoutPickerOpen(open: boolean): void;
}

export const useUi = create<UiState>((set) => ({
  theme: readThemePreference(THEME_KEY),
  palette: null,
  newProjectOpen: false,
  settingsOpen: false,
  layoutPickerOpen: false,
  setTheme: (theme) => {
    writeThemePreference(THEME_KEY, theme);
    set({ theme });
  },
  setPalette: (palette) => set({ palette }),
  setNewProjectOpen: (newProjectOpen) => set({ newProjectOpen }),
  setSettingsOpen: (settingsOpen) => set({ settingsOpen }),
  setLayoutPickerOpen: (layoutPickerOpen) => set({ layoutPickerOpen }),
}));
