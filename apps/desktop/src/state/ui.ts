import { create } from "zustand";

export type ThemePreference = "system" | "light" | "dark";

export interface Toast {
  id: number;
  kind: "info" | "error" | "success";
  text: string;
}

export type PaletteMode = "commands" | "files" | "goto" | null;

export type UnsavedChoice = "save" | "discard" | "cancel";

export interface UnsavedPrompt {
  title: string;
  message: string;
  resolve: (choice: UnsavedChoice) => void;
}

interface UiState {
  theme: ThemePreference;
  palette: PaletteMode;
  newSessionOpen: boolean;
  settingsOpen: boolean;
  toasts: Toast[];
  /** The "unsaved changes" question currently shown, if any. */
  unsavedPrompt: UnsavedPrompt | null;
  /** Asks what to do with unsaved changes; resolves "cancel" if another question is already open. */
  askUnsaved(title: string, message: string): Promise<UnsavedChoice>;
  setTheme(theme: ThemePreference): void;
  setPalette(mode: PaletteMode): void;
  setNewSessionOpen(open: boolean): void;
  setSettingsOpen(open: boolean): void;
  toast(kind: Toast["kind"], text: string): void;
  dismissToast(id: number): void;
}

let toastId = 0;

function readTheme(): ThemePreference {
  try {
    const v = localStorage.getItem("analyst.theme");
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    // storage unavailable
  }
  return "system";
}

export const useUi = create<UiState>((set, get) => ({
  theme: readTheme(),
  palette: null,
  newSessionOpen: false,
  settingsOpen: false,
  toasts: [],
  unsavedPrompt: null,
  askUnsaved: (title, message) =>
    new Promise<UnsavedChoice>((resolve) => {
      if (get().unsavedPrompt) {
        resolve("cancel");
        return;
      }
      set({
        unsavedPrompt: {
          title,
          message,
          resolve: (choice) => {
            set({ unsavedPrompt: null });
            resolve(choice);
          },
        },
      });
    }),
  setTheme: (theme) => {
    try {
      localStorage.setItem("analyst.theme", theme);
    } catch {
      // storage unavailable
    }
    set({ theme });
  },
  setPalette: (palette) => set({ palette }),
  setNewSessionOpen: (newSessionOpen) => set({ newSessionOpen }),
  setSettingsOpen: (settingsOpen) => set({ settingsOpen }),
  toast: (kind, text) => {
    const id = ++toastId;
    set((s) => ({ toasts: [...s.toasts, { id, kind, text }] }));
    setTimeout(() => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })), kind === "error" ? 8000 : 4000);
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

export function notifyError(e: unknown): void {
  const text = e instanceof Error ? e.message : typeof e === "object" && e && "message" in e ? String((e as { message: unknown }).message) : String(e);
  useUi.getState().toast("error", text);
}
