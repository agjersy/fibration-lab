// Zed-style keymap (subset). `mod` is Cmd on macOS and Ctrl elsewhere; `ctrl`
// is Control everywhere. Keys are lower-case; `+` is not used so the strings
// stay unambiguous for keys like "-".
//
// Contexts
//   global    anywhere outside the special contexts below
//   editor    focus inside a CodeMirror editor (global bindings also apply)
//   chat      focus inside the agent chat input (global bindings also apply)
//   terminal  focus inside an xterm. The shell owns the keyboard there: Ctrl+R,
//             Ctrl+W, Ctrl+L, Ctrl+P, Ctrl+B, Ctrl+J, Ctrl+G, Ctrl+S and friends
//             must reach readline. Only an explicit allow-list of global
//             bindings stays active (see `allowedInTerminal`):
//               - `new_terminal` (Ctrl+`),
//               - chords with Shift plus Ctrl or Cmd (shells do not use them),
//               - on macOS, chords with Cmd (the terminal never sees Cmd).
//
// Guards (`when`)
//   closable_tab  `close_tab` (mod-w) applies only while a closable tab (an
//             editor or a proposal review) is focused; the handler then calls
//             preventDefault and closes that tab, after the unsaved-changes
//             question. In every other situation the chord is NOT bound and
//             NOT prevented: on macOS Cmd+W is the platform's "close window"
//             and stays that (the window-close guard asks about unsaved
//             buffers), and in a terminal on Linux/Windows Ctrl+W deletes a
//             word in the shell.
//
// The global handler (ui/useAppCommands.ts) also ignores every binding while a
// modal dialog is open, so shortcuts cannot act on the workbench behind it.

export type KeyContext = "global" | "editor" | "chat" | "terminal";

export type KeyGuard = "closable_tab";

export interface KeyBinding {
  command: string;
  key: string;
  context: Exclude<KeyContext, "terminal">;
  when?: KeyGuard;
}

export const KEYMAP: readonly KeyBinding[] = [
  { command: "file_finder", key: "mod-p", context: "global" },
  { command: "command_palette", key: "mod-shift-p", context: "global" },
  { command: "save", key: "mod-s", context: "global" },
  { command: "toggle_left_dock", key: "mod-b", context: "global" },
  { command: "toggle_bottom_dock", key: "mod-j", context: "global" },
  { command: "toggle_right_dock", key: "mod-r", context: "global" },
  { command: "focus_chat", key: "mod-l", context: "global" },
  { command: "new_terminal", key: "ctrl-`", context: "global" },
  { command: "close_tab", key: "mod-w", context: "global", when: "closable_tab" },
  { command: "next_tab", key: "ctrl-tab", context: "global" },
  { command: "prev_tab", key: "ctrl-shift-tab", context: "global" },
  { command: "go_to_line", key: "ctrl-g", context: "global" },
  { command: "run_current_script", key: "mod-enter", context: "editor" },
  { command: "chat_send", key: "mod-enter", context: "chat" },
];

export function isMac(platform: string = typeof navigator === "undefined" ? "" : navigator.platform): boolean {
  return /mac|iphone|ipad/i.test(platform);
}

export interface ParsedKey {
  mod: boolean;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  key: string;
}

export function parseKey(spec: string): ParsedKey {
  const parts = spec.toLowerCase().split("-");
  let key = parts.pop() ?? "";
  // "mod--" binds the minus key: the split leaves two empty strings.
  if (key === "" && parts[parts.length - 1] === "") {
    parts.pop();
    key = "-";
  }
  const p: ParsedKey = { mod: false, ctrl: false, shift: false, alt: false, key };
  for (const m of parts) {
    if (m === "mod") p.mod = true;
    else if (m === "ctrl") p.ctrl = true;
    else if (m === "shift") p.shift = true;
    else if (m === "alt") p.alt = true;
    else throw new Error(`unknown modifier ${m} in ${spec}`);
  }
  return p;
}

export interface KeyLike {
  key: string;
  /** Physical key (`KeyboardEvent.code`); used when `key` is not a Latin character. */
  code?: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

const CODE_KEYS: Record<string, string> = {
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

function keyFromCode(code: string | undefined): string | null {
  if (!code) return null;
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter && letter[1]) return letter[1].toLowerCase();
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit && digit[1]) return digit[1];
  return CODE_KEYS[code] ?? null;
}

/**
 * The key a binding is compared with. On layouts that do not produce Latin
 * characters (Cyrillic, Greek, Hebrew, ...) and for dead keys, `key` is not
 * what the binding names, so the physical key position (`code`) is used
 * instead: Ctrl+З on a Russian layout is Ctrl+P.
 */
export function normalizeEventKey(e: Pick<KeyLike, "key" | "code">): string {
  const key = e.key;
  if (key === " ") return "space";
  if (key.length === 1) {
    if (key.charCodeAt(0) < 128) return key.toLowerCase();
    return keyFromCode(e.code) ?? key.toLowerCase();
  }
  if (key === "Dead" || key === "Unidentified" || key === "Process") return keyFromCode(e.code) ?? (key === "Dead" ? "`" : key.toLowerCase());
  return key.toLowerCase();
}

export function matchesKey(e: KeyLike, spec: string, mac: boolean = isMac()): boolean {
  const p = parseKey(spec);
  const wantMeta = mac ? p.mod : false;
  const wantCtrl = mac ? p.ctrl : p.ctrl || p.mod;
  if (e.metaKey !== wantMeta) return false;
  if (e.ctrlKey !== wantCtrl) return false;
  if (e.shiftKey !== p.shift) return false;
  if (e.altKey !== p.alt) return false;
  return normalizeEventKey(e) === p.key;
}

/** Whether a global binding stays active while a terminal has focus (see the header). */
export function allowedInTerminal(binding: KeyBinding, mac: boolean = isMac()): boolean {
  if (binding.context !== "global") return false;
  if (binding.command === "new_terminal") return true;
  const p = parseKey(binding.key);
  if (p.shift && (p.ctrl || p.mod)) return true;
  return mac && p.mod;
}

export type GuardState = Partial<Record<KeyGuard, boolean>>;

/**
 * Resolves a key event to a command id for the given focus context. A binding
 * with a `when` guard only matches while that guard holds; otherwise the event
 * is left alone (not handled, not prevented).
 */
export function resolveKey(e: KeyLike, context: KeyContext, mac: boolean = isMac(), guards: GuardState = {}): string | null {
  const candidates =
    context === "terminal" ? KEYMAP.filter((b) => allowedInTerminal(b, mac)) : KEYMAP.filter((b) => b.context === context || b.context === "global");
  // Context-specific bindings win over global ones.
  candidates.sort((a, b) => (a.context === "global" ? 1 : 0) - (b.context === "global" ? 1 : 0));
  for (const b of candidates) {
    if (b.when && !guards[b.when]) continue;
    if (matchesKey(e, b.key, mac)) return b.command;
  }
  return null;
}

export function bindingFor(command: string): KeyBinding | undefined {
  return KEYMAP.find((b) => b.command === command);
}

const MAC_SYMBOLS: Record<string, string> = { mod: "⌘", ctrl: "⌃", shift: "⇧", alt: "⌥" };
const KEY_NAMES: Record<string, string> = { enter: "Enter", tab: "Tab", "`": "`", escape: "Esc", space: "Space" };

export function formatKey(spec: string, mac: boolean = isMac()): string {
  const p = parseKey(spec);
  const keyName = KEY_NAMES[p.key] ?? p.key.toUpperCase();
  if (mac) {
    return `${p.ctrl ? MAC_SYMBOLS.ctrl : ""}${p.alt ? MAC_SYMBOLS.alt : ""}${p.shift ? MAC_SYMBOLS.shift : ""}${p.mod ? MAC_SYMBOLS.mod : ""}${keyName}`;
  }
  const parts: string[] = [];
  if (p.ctrl || p.mod) parts.push("Ctrl");
  if (p.alt) parts.push("Alt");
  if (p.shift) parts.push("Shift");
  parts.push(keyName);
  return parts.join("+");
}
