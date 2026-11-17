// Zed-style keymap (subset), on the shared machinery of @wb/ui (ported from Personal Analyst
// apps/desktop/src/keymap.ts at commit f229e4d). `mod` is Cmd on macOS and Ctrl elsewhere.
import { createKeymap } from "@wb/ui";
import type { KeyBinding } from "@wb/ui";

/** Focus contexts: `text` is the in-place slide editor, `rail` the slide list, `chat` the chat input. */
export type KeyContext = "global" | "text" | "rail" | "chat";

export const KEYMAP: readonly KeyBinding<Exclude<KeyContext, "global">>[] = [
  { command: "slide_finder", key: "mod-p", context: "global" },
  { command: "command_palette", key: "mod-shift-p", context: "global" },
  { command: "toggle_left_dock", key: "mod-b", context: "global" },
  { command: "toggle_bottom_dock", key: "mod-j", context: "global" },
  { command: "toggle_right_dock", key: "mod-r", context: "global" },
  { command: "focus_chat", key: "mod-l", context: "global" },
  { command: "next_tab", key: "ctrl-tab", context: "global" },
  { command: "prev_tab", key: "ctrl-shift-tab", context: "global" },
  { command: "next_slide", key: "alt-arrowdown", context: "global" },
  { command: "prev_slide", key: "alt-arrowup", context: "global" },
  // In the slide editor Cmd/Ctrl+B, I, U format text; the editor handles them itself.
  { command: "text_bold", key: "mod-b", context: "text" },
  { command: "text_italic", key: "mod-i", context: "text" },
  { command: "text_underline", key: "mod-u", context: "text" },
  { command: "move_slide_down", key: "alt-arrowdown", context: "rail" },
  { command: "move_slide_up", key: "alt-arrowup", context: "rail" },
  { command: "chat_send", key: "mod-enter", context: "chat" },
];

export const keymap = createKeymap(KEYMAP);
