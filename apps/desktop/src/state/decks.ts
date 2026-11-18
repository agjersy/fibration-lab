// The decks of each session: the list (`decks_list`, `decks_changed`), the deck the deck panels
// show, and per deck its document, one SlideView per slide, the selection, diagnostics,
// references, exports and the source passage on display. Fed by `deck_get` and the other deck
// commands, and by the `deck` / `decks_changed` session events through the dispatcher.
import { create } from "zustand";
import { orderedSlideIds } from "../deck/model";
import type { Deck, DeckSummary, KernelState, SessionId } from "../ipc";

export interface SourceState {
  cite: string;
  passage: Deck.SourcePassage | null;
  error: string | null;
}

export interface DeckState {
  /** `deck_open`: template description, engines, sandbox. Null until the deck has been opened. */
  info: Deck.DeckInfo | null;
  deck: Deck.Deck | null;
  slides: Record<string, Deck.SlideView>;
  selected: string | null;
  layoutThumbs: Deck.LayoutThumbnail[];
  diagnostics: Deck.Diagnostic[];
  refs: Deck.RefView[];
  exports: Deck.ExportView[];
  source: SourceState | null;
  buildOutput: string;
  kernel: { state: KernelState; message: string | null } | null;
  /** The storyline (`.present/outline.json`): key message and sources by slide id. */
  outline: OutlineRow[];
}

export interface OutlineRow {
  slide_id?: string | null;
  title: string;
  message: string;
  layout?: string | null;
  sources?: string[];
}

export function deckKey(session: SessionId, deck: string): string {
  return `${session}\n${deck}`;
}

export function emptyDeck(): DeckState {
  return {
    info: null,
    deck: null,
    slides: {},
    selected: null,
    layoutThumbs: [],
    diagnostics: [],
    refs: [],
    exports: [],
    source: null,
    buildOutput: "",
    kernel: null,
    outline: [],
  };
}

interface DecksStore {
  lists: Record<SessionId, DeckSummary[]>;
  /** The deck the deck panels of a session show. */
  active: Record<SessionId, string | null>;
  decks: Record<string, DeckState>;
  setList(session: SessionId, decks: DeckSummary[]): void;
  setActive(session: SessionId, deck: string | null): void;
  /** Merges fields into one deck's state, creating it when needed. */
  patch(session: SessionId, deck: string, fields: Partial<DeckState> | ((d: DeckState) => Partial<DeckState>)): void;
  setDeck(session: SessionId, deck: string, doc: Deck.Deck, slides: Deck.SlideView[]): void;
  /** `deck_changed`: new order and sections; removed slides are dropped. */
  applyDeck(session: SessionId, deck: string, doc: Deck.Deck, removed: string[]): void;
  /** `slide_view` or a command result. */
  upsertSlide(session: SessionId, deck: string, view: Deck.SlideView): void;
  select(session: SessionId, deck: string, slideId: string | null): void;
  /** Diagnostics of the listed slides replace theirs (`diagnostics` event); `null` replaces all. */
  replaceDiagnostics(session: SessionId, deck: string, slides: string[] | null, diagnostics: Deck.Diagnostic[]): void;
  addExport(session: SessionId, deck: string, view: Deck.ExportView): void;
  dropSession(session: SessionId): void;
}

function fixSelection(deck: Deck.Deck, selected: string | null, previousOrder: string[]): string | null {
  const ids = orderedSlideIds(deck);
  if (selected && ids.includes(selected)) return selected;
  if (!selected) return ids[0] ?? null;
  // The selected slide was removed: fall to its neighbour in the old order.
  const at = previousOrder.indexOf(selected);
  for (let i = at; i >= 0; i--) {
    const id = previousOrder[i];
    if (id && ids.includes(id)) return id;
  }
  return ids[0] ?? null;
}

export const useDecks = create<DecksStore>((set) => {
  const update = (session: SessionId, deck: string, fn: (d: DeckState) => DeckState | null) =>
    set((s) => {
      const k = deckKey(session, deck);
      const next = fn(s.decks[k] ?? emptyDeck());
      return next ? { decks: { ...s.decks, [k]: next } } : s;
    });
  return {
    lists: {},
    active: {},
    decks: {},
    setList: (session, decks) => set((s) => ({ lists: { ...s.lists, [session]: decks } })),
    setActive: (session, deck) => set((s) => (s.active[session] === deck ? s : { active: { ...s.active, [session]: deck } })),
    patch: (session, deck, fields) => update(session, deck, (d) => ({ ...d, ...(typeof fields === "function" ? fields(d) : fields) })),
    setDeck: (session, deck, doc, slides) =>
      update(session, deck, (d) => ({
        ...d,
        deck: doc,
        slides: Object.fromEntries(slides.map((v) => [v.slide.id, v])),
        selected: fixSelection(doc, d.selected, d.deck ? orderedSlideIds(d.deck) : []),
      })),
    applyDeck: (session, deck, doc, removed) =>
      update(session, deck, (d) => {
        const slides = { ...d.slides };
        for (const id of removed) delete slides[id];
        // Indexes and sections follow the new order even before the slide views arrive.
        const order = orderedSlideIds(doc);
        for (const section of doc.sections) {
          for (const id of section.slides) {
            const v = slides[id];
            const index = order.indexOf(id);
            if (v && (v.index !== index || v.section !== section.title)) slides[id] = { ...v, index, section: section.title };
          }
        }
        const diagnostics = removed.length ? d.diagnostics.filter((x) => !x.slide_id || !removed.includes(x.slide_id)) : d.diagnostics;
        return { ...d, deck: doc, slides, diagnostics, selected: fixSelection(doc, d.selected, d.deck ? orderedSlideIds(d.deck) : []) };
      }),
    upsertSlide: (session, deck, view) => update(session, deck, (d) => ({ ...d, slides: { ...d.slides, [view.slide.id]: view } })),
    select: (session, deck, slideId) => update(session, deck, (d) => (d.selected === slideId ? null : { ...d, selected: slideId })),
    replaceDiagnostics: (session, deck, slides, diagnostics) =>
      update(session, deck, (d) => {
        if (slides === null) return { ...d, diagnostics };
        const kept = d.diagnostics.filter((x) => !x.slide_id || !slides.includes(x.slide_id));
        return { ...d, diagnostics: [...kept, ...diagnostics] };
      }),
    addExport: (session, deck, view) => update(session, deck, (d) => ({ ...d, exports: [view, ...d.exports.filter((x) => x.id !== view.id)] })),
    dropSession: (session) =>
      set((s) => {
        const decks = Object.fromEntries(Object.entries(s.decks).filter(([k]) => !k.startsWith(`${session}\n`)));
        const { [session]: _list, ...lists } = s.lists;
        const { [session]: _active, ...active } = s.active;
        void _list;
        void _active;
        return { decks, lists, active };
      }),
  };
});

const NO_DECKS: DeckSummary[] = [];

export function useDeckList(session: SessionId | null): DeckSummary[] {
  return useDecks((s) => (session ? (s.lists[session] ?? NO_DECKS) : NO_DECKS));
}

export function useActiveDeckSlug(session: SessionId | null): string | null {
  return useDecks((s) => (session ? (s.active[session] ?? null) : null));
}

export function useDeckState(session: SessionId | null, deck: string | null): DeckState | null {
  return useDecks((s) => (session && deck ? (s.decks[deckKey(session, deck)] ?? null) : null));
}

export function getDeckState(session: SessionId, deck: string): DeckState | null {
  return useDecks.getState().decks[deckKey(session, deck)] ?? null;
}

/** The slide views of a deck in deck order (slides whose view has not arrived yet are skipped). */
export function orderedViews(d: DeckState): Deck.SlideView[] {
  if (!d.deck) return [];
  return orderedSlideIds(d.deck).flatMap((id) => (d.slides[id] ? [d.slides[id]] : []));
}

export function selectedView(d: DeckState | null): Deck.SlideView | null {
  return d && d.selected ? (d.slides[d.selected] ?? null) : null;
}

/** Element ids of a slide the checker reports as `stale` (a run citation whose code has changed). */
export function staleElements(d: DeckState | null, slideId: string): Set<string> {
  const out = new Set<string>();
  for (const x of d?.diagnostics ?? []) if (x.rule === "stale" && x.slide_id === slideId && x.element_id) out.add(x.element_id);
  return out;
}
