import { describe, expect, it } from "vitest";
import { parseKey } from "@wb/ui";
import { KEYMAP, keymap } from "./keymap";

const ev = (key: string, mods: Partial<{ metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean }> = {}) => ({ key, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...mods });

describe("keymap table", () => {
  it("has parseable keys and no chord bound twice within one context", () => {
    const byContext = new Map<string, Set<string>>();
    for (const b of KEYMAP) {
      expect(() => parseKey(b.key)).not.toThrow();
      const set = byContext.get(b.context) ?? new Set();
      expect(set.has(b.key)).toBe(false);
      set.add(b.key);
      byContext.set(b.context, set);
    }
  });

  it("covers the Zed subset shared with Analyst", () => {
    const expected: Record<string, string> = { command_palette: "mod-shift-p", toggle_left_dock: "mod-b", toggle_bottom_dock: "mod-j", toggle_right_dock: "mod-r", focus_chat: "mod-l", next_tab: "ctrl-tab", prev_tab: "ctrl-shift-tab" };
    for (const [cmd, key] of Object.entries(expected)) expect(keymap.bindingFor(cmd)?.key).toBe(key);
  });

  it("gives Ctrl+B to the text editor while editing and to the dock otherwise", () => {
    expect(keymap.resolveKey(ev("b", { ctrlKey: true }), "text", false)).toBe("text_bold");
    expect(keymap.resolveKey(ev("b", { ctrlKey: true }), "global", false)).toBe("toggle_left_dock");
    expect(keymap.resolveKey(ev("ArrowDown", { altKey: true }), "rail", false)).toBe("move_slide_down");
    expect(keymap.resolveKey(ev("ArrowDown", { altKey: true }), "global", false)).toBe("next_slide");
  });
});
