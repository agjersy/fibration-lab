import { openDeckPanel } from "../workbench/controller";

/** What a deck panel shows while no deck is open in the session. */
export function NoDeck() {
  return (
    <div className="empty" data-testid="no-deck">
      No deck open.{" "}
      <button type="button" className="small" onClick={() => openDeckPanel("decks")}>
        Show decks
      </button>
    </div>
  );
}
