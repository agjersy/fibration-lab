// The deck of each open project: the deck document, one SlideView per slide, and the selection.
import { create } from "zustand";
import { orderedSlideIds } from "../deck/model";
import type { Deck, LayoutThumbnail, ProjectId, SlideView } from "../ipc";

export interface ProjectDeck {
  deck: Deck;
  slides: Record<string, SlideView>;
  selected: string | null;
  layoutThumbs: LayoutThumbnail[];
}

interface DeckState {
  decks: Record<ProjectId, ProjectDeck>;
  setDeck(project: ProjectId, deck: Deck, slides: SlideView[]): void;
  /** `deck_changed`: new order and sections; removed slides are dropped. */
  applyDeck(project: ProjectId, deck: Deck, removed: string[]): void;
  /** `slide_view` or a command result. */
  upsertSlide(project: ProjectId, view: SlideView): void;
  select(project: ProjectId, slideId: string | null): void;
  setLayoutThumbs(project: ProjectId, thumbs: LayoutThumbnail[]): void;
  drop(project: ProjectId): void;
}

function fixSelection(deck: Deck, selected: string | null, previousOrder: string[]): string | null {
  const ids = orderedSlideIds(deck);
  if (selected && ids.includes(selected)) return selected;
  if (!selected) return ids[0] ?? null;
  // The selected slide was removed: fall to its neighbour in the old order.
  const at = previousOrder.indexOf(selected);
  for (let i = at; i >= 0; i--) if (previousOrder[i] && ids.includes(previousOrder[i]!)) return previousOrder[i]!;
  return ids[0] ?? null;
}

export const useDeck = create<DeckState>((set) => ({
  decks: {},
  setDeck: (project, deck, slides) =>
    set((s) => {
      const prev = s.decks[project];
      const next: ProjectDeck = {
        deck,
        slides: Object.fromEntries(slides.map((v) => [v.slide.id, v])),
        selected: fixSelection(deck, prev?.selected ?? null, prev ? orderedSlideIds(prev.deck) : []),
        layoutThumbs: prev?.layoutThumbs ?? [],
      };
      return { decks: { ...s.decks, [project]: next } };
    }),
  applyDeck: (project, deck, removed) =>
    set((s) => {
      const prev = s.decks[project];
      if (!prev) return s;
      const slides = { ...prev.slides };
      for (const id of removed) delete slides[id];
      // Indexes and sections follow the new order even before the slide_view events arrive.
      for (const section of deck.sections) {
        for (const id of section.slides) {
          const v = slides[id];
          const index = orderedSlideIds(deck).indexOf(id);
          if (v && (v.index !== index || v.section !== section.title)) slides[id] = { ...v, index, section: section.title };
        }
      }
      return { decks: { ...s.decks, [project]: { ...prev, deck, slides, selected: fixSelection(deck, prev.selected, orderedSlideIds(prev.deck)) } } };
    }),
  upsertSlide: (project, view) =>
    set((s) => {
      const prev = s.decks[project];
      if (!prev) return s;
      return { decks: { ...s.decks, [project]: { ...prev, slides: { ...prev.slides, [view.slide.id]: view } } } };
    }),
  select: (project, slideId) =>
    set((s) => {
      const prev = s.decks[project];
      if (!prev || prev.selected === slideId) return s;
      return { decks: { ...s.decks, [project]: { ...prev, selected: slideId } } };
    }),
  setLayoutThumbs: (project, layoutThumbs) =>
    set((s) => {
      const prev = s.decks[project];
      return prev ? { decks: { ...s.decks, [project]: { ...prev, layoutThumbs } } } : s;
    }),
  drop: (project) =>
    set((s) => {
      const { [project]: _gone, ...decks } = s.decks;
      void _gone;
      return { decks };
    }),
}));

export function useProjectDeck(project: ProjectId | null): ProjectDeck | null {
  return useDeck((s) => (project ? (s.decks[project] ?? null) : null));
}

/** The slide views of a project in deck order (slides whose view has not arrived yet are skipped). */
export function orderedViews(d: ProjectDeck): SlideView[] {
  return orderedSlideIds(d.deck).flatMap((id) => (d.slides[id] ? [d.slides[id]] : []));
}

export function selectedView(d: ProjectDeck | null): SlideView | null {
  return d && d.selected ? (d.slides[d.selected] ?? null) : null;
}
