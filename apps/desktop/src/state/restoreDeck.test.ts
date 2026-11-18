// A restored workbench reopens the deck the session had open (SessionInfo.active_deck), or shows
// the Decks list when that deck no longer exists.
import { afterEach, describe, expect, it, vi } from "vitest";
import { setBackend } from "../ipc";
import type { DeckSummary } from "../ipc";
import { FakeBackend } from "../test/fake";
import { demoDeck } from "../test/deck/fixtures";
import { restoreActiveDeck } from "./deckActions";
import { deckKey, useDecks } from "./decks";
import { useSessions } from "./sessions";

const opened: string[] = [];
vi.mock("../ui/workbench/controller", async (actual) => ({
  ...(await actual<Record<string, unknown>>()),
  openDeckPanel: (id: string) => opened.push(id),
}));

const summary = { slug: "q3", title: "Q3", mode: "structured", slides: 0, template: "plain" } as unknown as DeckSummary;

function setup(activeDeck: string | null, decks: DeckSummary[]): FakeBackend {
  const fake = new FakeBackend();
  const info = fake.seed({ id: "s1", open: true });
  useSessions.getState().setInfo({ ...info, active_deck: activeDeck });
  fake.handle("decks_list", () => decks as never);
  fake.handle("deck_open", () => ({ kernel: "ready" }) as never);
  fake.handle("deck_get", () => ({ deck: demoDeck(), slides: [] }) as never);
  setBackend(fake);
  return fake;
}

afterEach(() => {
  opened.length = 0;
  useDecks.setState({ lists: {}, active: {}, decks: {} });
});

describe("restoreActiveDeck", () => {
  it("reopens the deck the session had open", async () => {
    setup("q3", [summary]);
    await restoreActiveDeck("s1");
    expect(useDecks.getState().active["s1"]).toBe("q3");
    expect(Object.keys(useDecks.getState().decks)).toContain(deckKey("s1", "q3"));
    expect(opened).toEqual([]);
  });

  it("shows the Decks list when that deck no longer exists", async () => {
    setup("gone", [summary]);
    await restoreActiveDeck("s1");
    expect(useDecks.getState().active["s1"]).toBeUndefined();
    expect(opened).toEqual(["decks"]);
  });

  it("does nothing when the session had no deck open", async () => {
    setup(null, [summary]);
    await restoreActiveDeck("s1");
    expect(useDecks.getState().active["s1"]).toBeUndefined();
    expect(opened).toEqual([]);
  });
});
