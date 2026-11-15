import { describe, expect, it } from "vitest";
import type { KeyLike } from "./keymap";
import { KEYMAP, allowedInTerminal, bindingFor, formatKey, matchesKey, normalizeEventKey, parseKey, resolveKey } from "./keymap";

const ev = (key: string, mods: Partial<{ metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean }> = {}): KeyLike => ({
  key,
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  ...mods,
});

describe("terminal context", () => {
  const ctrl = (key: string, code?: string): KeyLike => ({ ...ev(key, { ctrlKey: true }), code });

  it("passes shell control keys through on Linux and Windows", () => {
    for (const key of ["r", "w", "l", "p", "b", "j", "g", "s"]) {
      expect(resolveKey(ctrl(key), "terminal", false, { closable_tab: true })).toBeNull();
    }
    expect(resolveKey(ev("Tab", { ctrlKey: true }), "terminal", false)).toBeNull();
    // The same chords are bound outside the terminal.
    expect(resolveKey(ctrl("r"), "global", false)).toBe("toggle_right_dock");
    expect(resolveKey(ctrl("p"), "editor", false)).toBe("file_finder");
  });

  it("keeps the explicit allow-list active", () => {
    expect(resolveKey(ctrl("`"), "terminal", false)).toBe("new_terminal");
    expect(resolveKey(ev("P", { ctrlKey: true, shiftKey: true }), "terminal", false)).toBe("command_palette");
    expect(resolveKey(ev("Tab", { ctrlKey: true, shiftKey: true }), "terminal", false)).toBe("prev_tab");
    expect(
      KEYMAP.filter((b) => allowedInTerminal(b, false))
        .map((b) => b.command)
        .sort(),
    ).toEqual(["command_palette", "new_terminal", "prev_tab"]);
  });

  it("on macOS Cmd chords stay with the app and Ctrl chords go to the shell", () => {
    expect(resolveKey(ev("p", { metaKey: true }), "terminal", true)).toBe("file_finder");
    expect(resolveKey(ev("s", { metaKey: true }), "terminal", true)).toBe("save");
    expect(resolveKey(ev("r", { ctrlKey: true }), "terminal", true)).toBeNull();
    expect(resolveKey(ev("g", { ctrlKey: true }), "terminal", true)).toBeNull();
    expect(resolveKey(ev("`", { ctrlKey: true }), "terminal", true)).toBe("new_terminal");
  });
});

describe("close_tab guard", () => {
  it("binds mod-w only while a closable tab is focused", () => {
    expect(bindingFor("close_tab")?.when).toBe("closable_tab");
    expect(resolveKey(ev("w", { metaKey: true }), "editor", true, { closable_tab: true })).toBe("close_tab");
    // No tab to close: Cmd+W is left to the platform (not resolved, so not prevented).
    expect(resolveKey(ev("w", { metaKey: true }), "global", true, { closable_tab: false })).toBeNull();
    expect(resolveKey(ev("w", { metaKey: true }), "global", true)).toBeNull();
    expect(resolveKey(ev("w", { ctrlKey: true }), "global", false, { closable_tab: true })).toBe("close_tab");
    expect(resolveKey(ev("w", { ctrlKey: true }), "terminal", false, { closable_tab: true })).toBeNull();
  });
});

describe("non-Latin layouts", () => {
  it("falls back to the physical key when the key is not a Latin character", () => {
    // Russian layout: the P key produces "з".
    expect(normalizeEventKey({ key: "з", code: "KeyP" })).toBe("p");
    expect(resolveKey({ ...ev("з", { ctrlKey: true }), code: "KeyP" }, "global", false)).toBe("file_finder");
    expect(resolveKey({ ...ev("З", { ctrlKey: true, shiftKey: true }), code: "KeyP" }, "global", false)).toBe("command_palette");
    expect(resolveKey({ ...ev("ы", { metaKey: true }), code: "KeyS" }, "editor", true)).toBe("save");
    // Greek, and a dead key on the backquote position.
    expect(normalizeEventKey({ key: "ς", code: "KeyW" })).toBe("w");
    expect(resolveKey({ ...ev("Dead", { ctrlKey: true }), code: "Backquote" }, "global", false)).toBe("new_terminal");
  });

  it("trusts Latin keys over the physical position (Dvorak, AZERTY)", () => {
    // Dvorak: the physical R key produces "p".
    expect(normalizeEventKey({ key: "p", code: "KeyR" })).toBe("p");
    expect(resolveKey({ ...ev("p", { ctrlKey: true }), code: "KeyR" }, "global", false)).toBe("file_finder");
    expect(normalizeEventKey({ key: "Enter", code: "Enter" })).toBe("enter");
  });
});

describe("keymap table", () => {
  it("has unique command ids per context and parseable keys", () => {
    const seen = new Set<string>();
    for (const b of KEYMAP) {
      const id = `${b.context}:${b.command}`;
      expect(seen.has(id)).toBe(false);
      seen.add(id);
      expect(() => parseKey(b.key)).not.toThrow();
    }
  });

  it("does not bind the same chord twice within one context", () => {
    const byContext = new Map<string, Set<string>>();
    for (const b of KEYMAP) {
      const set = byContext.get(b.context) ?? new Set();
      expect(set.has(b.key)).toBe(false);
      set.add(b.key);
      byContext.set(b.context, set);
    }
  });

  it("covers the Zed subset", () => {
    const expected: Record<string, string> = {
      file_finder: "mod-p",
      command_palette: "mod-shift-p",
      save: "mod-s",
      toggle_left_dock: "mod-b",
      toggle_bottom_dock: "mod-j",
      toggle_right_dock: "mod-r",
      focus_chat: "mod-l",
      new_terminal: "ctrl-`",
      close_tab: "mod-w",
      next_tab: "ctrl-tab",
      prev_tab: "ctrl-shift-tab",
      go_to_line: "ctrl-g",
      run_current_script: "mod-enter",
    };
    for (const [cmd, key] of Object.entries(expected)) expect(bindingFor(cmd)?.key).toBe(key);
  });
});

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

  it("prefers context bindings over global ones", () => {
    expect(resolveKey(ev("Enter", { ctrlKey: true }), "editor", false)).toBe("run_current_script");
    expect(resolveKey(ev("Enter", { ctrlKey: true }), "chat", false)).toBe("chat_send");
    expect(resolveKey(ev("Enter", { ctrlKey: true }), "global", false)).toBeNull();
    expect(resolveKey(ev("s", { ctrlKey: true }), "editor", false)).toBe("save");
    expect(resolveKey(ev("P", { ctrlKey: true, shiftKey: true }), "global", false)).toBe("command_palette");
  });

  it("formats bindings per platform", () => {
    expect(formatKey("mod-shift-p", true)).toBe("⇧⌘P");
    expect(formatKey("mod-shift-p", false)).toBe("Ctrl+Shift+P");
    expect(formatKey("ctrl-`", true)).toBe("⌃`");
    expect(formatKey("mod-enter", false)).toBe("Ctrl+Enter");
  });
});
