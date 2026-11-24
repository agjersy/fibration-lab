// The deck store and its event path: deck events of the session channel update one deck of one
// session; deck paths load through the asset protocol under Tauri only.
import { afterEach, describe, expect, it, vi } from "vitest";
import { deckFileUrl } from "../deck/files";
import { setBackend } from "../ipc";
import { FakeBackend } from "../test/fake";
import { waitFor } from "../test/util";
import type { Deck } from "../ipc";
import { demoDeck, demoSlides } from "../test/deck/fixtures";
import { applyDeckEvent } from "./deckEvents";
import { deckKey, getDeckState, orderedViews, staleElements, useDecks } from "./decks";
import { useTasks } from "./tasks";

const S = "s-1";
const D = "q3-review";

function view(slide: Deck.Slide, index: number): Deck.SlideView {
  return { slide, hash: `h-${slide.id}`, index, section: "", thumb: { kind: "preview", path: null, stale: true }, checks: { errors: 0, warnings: 0 }, numbers: { green: 0, amber: 0, red: 0, grey: 0 } };
}

function load(): void {
  const slides = demoSlides();
  useDecks.getState().setDeck(S, D, demoDeck(), slides.map(view));
}

afterEach(() => {
  useDecks.setState({ lists: {}, active: {}, decks: {} });
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

describe("deck store", () => {
  it("keys decks by session and deck and selects the first slide", () => {
    load();
    const d = getDeckState(S, D)!;
    expect(Object.keys(useDecks.getState().decks)).toEqual([deckKey(S, D)]);
    expect(d.selected).toBe("s-title");
    expect(orderedViews(d).map((v) => v.slide.id)[0]).toBe("s-title");
    expect(getDeckState("other", D)).toBeNull();
  });

  it("applies deck_changed: new order, removed slides dropped, the selection falls to a neighbour", () => {
    load();
    useDecks.getState().select(S, D, "s-revenue");
    const doc = demoDeck();
    doc.sections = doc.sections.map((s) => ({ ...s, slides: s.slides.filter((id) => id !== "s-revenue") }));
    applyDeckEvent(S, D, { type: "deck_changed", deck: doc, changed: [], removed: ["s-revenue"], by: "user" });
    const d = getDeckState(S, D)!;
    expect(d.slides["s-revenue"]).toBeUndefined();
    expect(d.selected).toBe("s-margin");
    expect(d.slides["s-regions"]!.index).toBe(2);
  });

  it("applies slide views, diagnostics by slide, exports and task progress", () => {
    load();
    const slide = { ...demoSlides()[0]!, hidden: true };
    applyDeckEvent(S, D, { type: "slide_view", view: view(slide, 0) });
    expect(getDeckState(S, D)!.slides["s-title"]!.slide.hidden).toBe(true);

    const stale: Deck.Diagnostic = { rule: "stale", severity: "warn", message: "run changed", slide_id: "s-site", element_id: "e-img" };
    applyDeckEvent(S, D, { type: "diagnostics", slides: ["s-site"], diagnostics: [stale] });
    applyDeckEvent(S, D, { type: "diagnostics", slides: ["s-title"], diagnostics: [] });
    expect(getDeckState(S, D)!.diagnostics).toEqual([stale]);
    expect([...staleElements(getDeckState(S, D), "s-site")]).toEqual(["e-img"]);
    expect(staleElements(getDeckState(S, D), "s-title").size).toBe(0);

    const x: Deck.ExportView = { id: "x-1", format: "pptx", preset: null, path: "exports/q3.pptx", checkpoint: "cp-1", created_at: "2026-11-18T00:00:00Z", errors: 0, warnings: 0, engine: null };
    applyDeckEvent(S, D, { type: "export_done", export: x });
    applyDeckEvent(S, D, { type: "export_done", export: x });
    expect(getDeckState(S, D)!.exports).toEqual([x]);

    applyDeckEvent(S, D, { type: "task_progress", task_id: "deck-task-1", fraction: null, message: "Exporting pptx" });
    applyDeckEvent(S, D, { type: "task_done", task_id: "deck-task-1", ok: true, message: "exports/q3.pptx" });
    expect(useTasks.getState().tasks["deck-task-1"]).toMatchObject({ session: S, label: "Exporting pptx", done: true, ok: true });
  });

  it("ignores slide events of a deck that was never opened", () => {
    applyDeckEvent(S, "unknown", { type: "slide_view", view: view(demoSlides()[0]!, 0) });
    expect(getDeckState(S, "unknown")).toBeNull();
  });
});

describe("deck file URLs", () => {
  it("passes data URLs through, fetches files over the bridge in browser mode, and uses the asset protocol under Tauri", async () => {
    expect(deckFileUrl(S, "/r", D, "data:image/png;base64,AAAA")).toBe("data:image/png;base64,AAAA");
    // Browser mode: null at first, then the file read through the bridge (repo-relative).
    const b = new FakeBackend();
    const asked: string[] = [];
    b.handle("fs_read_binary", ({ path }) => {
      asked.push(path);
      return { mime: "image/png", base64: "QUJD" };
    });
    setBackend(b);
    expect(deckFileUrl(S, "/r", D, "assets/abc.png")).toBeNull();
    expect(deckFileUrl(S, "/r", D, "/r/decks/q3-review/.present/cache/1.png")).toBeNull();
    await waitFor(() => deckFileUrl(S, "/r", D, "assets/abc.png"));
    expect(deckFileUrl(S, "/r", D, "assets/abc.png")).toBe("data:image/png;base64,QUJD");
    await waitFor(() => asked.length === 2);
    expect(asked).toEqual(["decks/q3-review/assets/abc.png", "decks/q3-review/.present/cache/1.png"]);
    // A path outside the repository is not asked for.
    expect(deckFileUrl(S, "/r", D, "/etc/passwd")).toBeNull();
    expect(asked).toHaveLength(2);

    const convert = vi.fn((path: string, protocol: string) => `${protocol}://localhost/${encodeURIComponent(path)}`);
    window.__TAURI_INTERNALS__ = { convertFileSrc: convert };
    expect(deckFileUrl(S, "/home/u/repo/", D, ".present/cache/thumbs/deck/1.png")).toBe(`asset://localhost/${encodeURIComponent("/home/u/repo/decks/q3-review/.present/cache/thumbs/deck/1.png")}`);
    expect(deckFileUrl(S, "/home/u/repo", D, "/abs/page.png")).toBe(`asset://localhost/${encodeURIComponent("/abs/page.png")}`);
    expect(deckFileUrl(S, null, D, "assets/x.png")).toBeNull();
  });
});
