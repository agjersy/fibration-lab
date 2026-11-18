// What every deck panel needs: the active session, the deck the panels show, its state, and a
// resolver from deck paths to loadable URLs.
import { useCallback } from "react";
import { deckFileUrl } from "../../deck/files";
import type { SessionId } from "../../ipc";
import { useActiveDeckSlug, useDeckState } from "../../state/decks";
import type { DeckState } from "../../state/decks";
import { useActiveSession, useSessionInfo } from "../../state/sessions";

export interface DeckContext {
  session: SessionId;
  deck: string;
  d: DeckState;
  resolve: (path: string | null | undefined) => string | null;
}

export function useDeckContext(): DeckContext | null {
  const session = useActiveSession();
  const info = useSessionInfo(session);
  const deck = useActiveDeckSlug(session);
  const d = useDeckState(session, deck);
  const repo = info?.repo_abs ?? null;
  const resolve = useCallback((path: string | null | undefined) => (session && deck ? deckFileUrl(session, repo, deck, path) : null), [session, deck, repo]);
  if (!session || !deck || !d) return null;
  return { session, deck, d, resolve };
}
