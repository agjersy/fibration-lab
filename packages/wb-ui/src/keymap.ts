// Ported from Fibration Lab apps/desktop/src/keymap.ts at commit f229e4d. The binding table is passed
// in by the app (`createKeymap`) instead of being a module constant; parsing, matching and formatting
// are unchanged.
//
// Zed-style keymap (subset). `mod` is Cmd on macOS and Ctrl elsewhere; `ctrl`
// is Control everywhere. Keys are lower-case; `+` is not used so the strings
// stay unambiguous for keys like "-".

export interface KeyBinding<Context extends string = string> {
  command: string;
  key: string;
  context: Context | "global";
}

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
  const key = parts.pop() ?? "";
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
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

function normalizeEventKey(key: string): string {
  if (key === " ") return "space";
  if (key === "Dead") return "`";
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
  return normalizeEventKey(e.key) === p.key;
}

const MAC_SYMBOLS: Record<string, string> = { mod: "⌘", ctrl: "⌃", shift: "⇧", alt: "⌥" };
const KEY_NAMES: Record<string, string> = {
  enter: "Enter",
  tab: "Tab",
  "`": "`",
  escape: "Esc",
  space: "Space",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
};

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

export interface Keymap<Context extends string = string> {
  bindings: readonly KeyBinding<Context>[];
  /** Resolves a key event to a command id for the given focus context. */
  resolveKey(e: KeyLike, context: Context | "global", mac?: boolean): string | null;
  bindingFor(command: string): KeyBinding<Context> | undefined;
  /** The display string of a command's shortcut, or null. */
  shortcut(command: string, mac?: boolean): string | null;
}

export function createKeymap<Context extends string>(bindings: readonly KeyBinding<Context>[]): Keymap<Context> {
  const bindingFor = (command: string) => bindings.find((b) => b.command === command);
  return {
    bindings,
    resolveKey(e, context, mac = isMac()) {
      const candidates = bindings.filter((b) => b.context === context || b.context === "global");
      // Context-specific bindings win over global ones.
      candidates.sort((a, b) => (a.context === "global" ? 1 : 0) - (b.context === "global" ? 1 : 0));
      for (const b of candidates) if (matchesKey(e, b.key, mac)) return b.command;
      return null;
    },
    bindingFor,
    shortcut(command, mac = isMac()) {
      const b = bindingFor(command);
      return b ? formatKey(b.key, mac) : null;
    },
  };
}
