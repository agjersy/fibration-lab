// @wb/ui: the workbench UI pieces shared by Personal Present and Personal Analyst. Nothing here may
// import app-specific code. Styles: `@wb/ui/styles/theme.css` (tokens) and `@wb/ui/styles/base.css`.
export type { Backend, BackendHolder, CommandMap, CommandSpec, IpcError } from "./backend";
export { createBackendHolder, errorMessage, hasTauri, ipcError, isIpcError } from "./backend";
export { MockClock, MockEmitter, sleep } from "./mock";
export type { MockTiming } from "./mock";
export type { KeyBinding, KeyLike, Keymap, ParsedKey } from "./keymap";
export { createKeymap, formatKey, isMac, matchesKey, parseKey } from "./keymap";
export type { CommandDef } from "./commands";
export { listCommands, onCommandsChanged, registerCommands, runCommand } from "./commands";
export type { Toast } from "./toasts";
export { notifyError, toast, useToasts } from "./toasts";
export type { ResolvedTheme, ThemePreference } from "./theme";
export { readThemePreference, useResolvedTheme, writeThemePreference } from "./theme";
export { compactJson, formatBytes, formatDuration, formatTime, shortArgs } from "./format";
export { ConfirmDialog, InputDialog, Modal } from "./components/Dialog";
export { ContextMenu } from "./components/ContextMenu";
export type { MenuItem } from "./components/ContextMenu";
export { Toasts } from "./components/Toasts";
export { Palette, commandItems, fuzzyScore, rankItems } from "./components/Palette";
export type { PaletteItem } from "./components/Palette";
