import { describe, expect, it } from "vitest";
import { MockBackend } from "./mock";
import type { Deck, SessionEvent } from "./types";

function fast(): MockBackend {
  return new MockBackend({ latency: [0, 0], timeScale: 0, seedSession: false });
}

function waitFor<T>(check: () => T | undefined, timeoutMs = 3000): Promise<T> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const tick = () => {
      const v = check();
      if (v !== undefined) resolve(v);
      else if (Date.now() - start > timeoutMs) reject(new Error("timeout waiting for condition"));
      else setTimeout(tick, 2);
    };
    tick();
  });
}

describe("MockBackend decks", () => {
  it("creates a deck, edits slides, and reports deck events on the session channel", async () => {
    const b = fast();
    const info = await b.invoke("session_create", { repo_abs: "/home/user/projects/decay-analysis", target: "local" });
    const events: SessionEvent[] = [];
    await b.subscribe(info.id, (e) => events.push(e));
    const session = info.id;

    expect(await b.invoke("decks_list", { session })).toEqual([]);
    const created = await b.invoke("deck_create", { session, title: "Q3 Review", template_abs: "/t/corp.potx" });
    expect(created).toMatchObject({ slug: "q3-review", mode: "structured", slides: 0, template: "corp.potx" });
    expect(events.some((e) => e.type === "decks_changed" && e.decks.length === 1)).toBe(true);
    await expect(b.invoke("deck_create", { session, title: "Q3 Review", template_abs: "/t/corp.potx" })).rejects.toMatchObject({ code: "conflict" });

    const deck = "q3-review";
    const opened = await b.invoke("deck_open", { session, deck });
    expect(opened.deck_mode).toBe("structured");
    const first = await b.invoke("slide_create", { session, deck, layout: "Title Slide" });
    const second = await b.invoke("slide_create", { session, deck, layout: "Title and Content", after: first.slide.id });
    expect((await b.invoke("deck_get", { session, deck })).slides.map((s) => s.slide.id)).toEqual([first.slide.id, second.slide.id]);

    const edited: Deck.Slide = { ...first.slide, layout: "Section Header" };
    const updated = await b.invoke("slide_update", { session, deck, slide: edited, expected_hash: first.hash });
    expect(updated.slide.layout).toBe("Section Header");
    await expect(b.invoke("slide_update", { session, deck, slide: edited, expected_hash: first.hash })).rejects.toMatchObject({ code: "conflict" });
    const deckEvents = events.filter((e) => e.type === "deck" && e.deck === deck);
    expect(deckEvents.length).toBeGreaterThanOrEqual(3);

    const task = await b.invoke("export_run", { session, deck, format: "pptx" });
    await waitFor(() => events.find((e) => e.type === "deck" && (e.event as Deck.DeckEvent).type === "task_done" && (e.event as { task_id: string }).task_id === task));
    expect(await b.invoke("exports_list", { session, deck })).toHaveLength(1);
    const history = await b.invoke("history_list", { session });
    expect(history[0]?.label).toBe("export pptx");

    await expect(b.invoke("deck_get", { session, deck: "missing" })).rejects.toMatchObject({ code: "not_found" });
  });

  it("rejects deck commands on a closed session", async () => {
    const b = fast();
    const info = await b.invoke("session_create", { repo_abs: "/r", target: "local" });
    await b.invoke("session_close", { session: info.id });
    await expect(b.invoke("decks_list", { session: info.id })).rejects.toMatchObject({ code: "not_found" });
  });
});
