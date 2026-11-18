// Applies one deck's events (`{type: "deck", deck, event}` on the session channel, IPC.md
// section 9) to the deck store. Called by the dispatcher.
import type { Deck, SessionId } from "../ipc";
import { reloadDeckSoon } from "./deckActions";
import { getDeckState, useDecks } from "./decks";
import { useTasks } from "./tasks";
import { useUi } from "./ui";

export function applyDeckEvent(session: SessionId, deck: string, e: Deck.DeckEvent): void {
  const store = useDecks.getState();
  const known = getDeckState(session, deck) !== null;
  switch (e.type) {
    case "deck_changed":
      if (!known) return;
      store.applyDeck(session, deck, e.deck, e.removed);
      // Changes the user did not make in this window (the agent's accepted proposals, a
      // restored checkpoint) carry no slide views: the deck is read again.
      if (e.by !== "user" && (e.changed.length > 0 || e.removed.length > 0)) reloadDeckSoon(session, deck);
      else if (e.changed.some((id) => !getDeckState(session, deck)?.slides[id])) reloadDeckSoon(session, deck);
      break;
    case "slide_view":
      if (known) store.upsertSlide(session, deck, e.view);
      break;
    case "diagnostics":
      store.replaceDiagnostics(session, deck, e.slides, e.diagnostics);
      break;
    case "refs_changed":
      store.patch(session, deck, { refs: e.refs });
      break;
    case "build_output":
      store.patch(session, deck, (d) => ({ buildOutput: (d.buildOutput + e.chunk).slice(-64_000) }));
      break;
    case "export_done":
      store.addExport(session, deck, e.export);
      break;
    case "kernel_state":
      store.patch(session, deck, { kernel: { state: e.state, message: e.message } });
      if (e.state === "error" && e.message) useUi.getState().toast("error", `Deck kernel error (${deck}): ${e.message}`);
      break;
    case "task_progress":
      if (!useTasks.getState().tasks[e.task_id]) useTasks.getState().start(session, e.task_id, e.message);
      break;
    case "task_done":
      useTasks.getState().done(e.task_id, e.ok, e.message);
      if (!e.ok && e.message) useUi.getState().toast("error", e.message);
      break;
    case "info_changed":
      store.patch(session, deck, { info: e.info });
      break;
  }
}
