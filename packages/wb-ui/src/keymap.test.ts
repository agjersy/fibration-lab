// Ported from Fibration Lab apps/desktop/src/keymap.test.ts at commit f229e4d (matching and
// formatting cases), against a local binding table.
import { describe, expect, it } from "vitest";
import { createKeymap, formatKey, matchesKey, parseKey } from "./keymap";

const ev = (key: string, mods: Partial<{ metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean }> = {}) => ({
  key,
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  ...mods,
});

const keymap = createKeymap<"editor" | "chat">([
  { command: "command_palette", key: "mod-shift-p", context: "global" },
  { command: "save", key: "mod-s", context: "global" },
  { command: "run", key: "mod-enter", context: "editor" },
  { command: "chat_send", key: "mod-enter", context: "chat" },
]);

describe("matching", () => {
  it("maps mod to Cmd on macOS and Ctrl elsewhere", () => {
    expect(matchesKey(ev("p", { metaKey: true }), "mod-p", true)).toBe(true);
    expect(matchesKey(ev("p", { ctrlKey: true }), "mod-p", true)).toBe(false);
    expect(matchesKey(ev("p", { ctrlKey: true }), "mod-p", false)).toBe(true);
    expect(matchesKey(ev("p", { metaKey: true }), "mod-p", false)).toBe(false);
  });

  it("keeps ctrl as Control on both platforms", () => {
    expect(matchesKey(ev("`", { ctrlKey: true }), "ctrl-`", true)).toBe(true);
    expect(matchesKey(ev("`", { ctrlKey: true }), "ctrl-`", false)).toBe(true);
    expect(matchesKey(ev("Tab", { ctrlKey: true, shiftKey: true }), "ctrl-shift-tab", false)).toBe(true);
    expect(matchesKey(ev("Tab", { ctrlKey: true }), "ctrl-shift-tab", false)).toBe(false);
  });

  it("rejects unknown modifiers", () => {
    expect(() => parseKey("hyper-p")).toThrow();
  });

  it("prefers context bindings over global ones", () => {
    expect(keymap.resolveKey(ev("Enter", { ctrlKey: true }), "editor", false)).toBe("run");
    expect(keymap.resolveKey(ev("Enter", { ctrlKey: true }), "chat", false)).toBe("chat_send");
    expect(keymap.resolveKey(ev("Enter", { ctrlKey: true }), "global", false)).toBeNull();
    expect(keymap.resolveKey(ev("s", { ctrlKey: true }), "editor", false)).toBe("save");
    expect(keymap.resolveKey(ev("P", { ctrlKey: true, shiftKey: true }), "global", false)).toBe("command_palette");
  });

  it("formats bindings per platform", () => {
    expect(formatKey("mod-shift-p", true)).toBe("⇧⌘P");
    expect(formatKey("mod-shift-p", false)).toBe("Ctrl+Shift+P");
    expect(formatKey("ctrl-`", true)).toBe("⌃`");
    expect(formatKey("mod-enter", false)).toBe("Ctrl+Enter");
    expect(keymap.shortcut("save", false)).toBe("Ctrl+S");
    expect(keymap.shortcut("missing", false)).toBeNull();
  });
});
