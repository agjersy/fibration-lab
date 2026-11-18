// One click from a slide to what it rests on: a run citation focuses the run (Runs view, Figures
// panel on that artifact, Run output with its script and arguments); a reference citation opens
// the source passage.
import type { Deck, SessionId } from "../../ipc";
import { openSource } from "../../state/deckActions";
import { useDecks } from "../../state/decks";
import { figureStem } from "../../state/figures";
import { useRuns } from "../../state/runs";
import { openDeckPanel, showPanel } from "../workbench/controller";

export function focusRunCite(session: SessionId, cite: Deck.RunCite): void {
  const runs = useRuns.getState();
  runs.selectRun(session, cite.run_id);
  runs.setActiveFigure(session, { name: figureStem({ path: cite.artifact, name: cite.artifact }), run_id: cite.run_id });
  showPanel("runoutput");
  showPanel("runs");
  showPanel("figures");
}

export function openCite(session: SessionId, deck: string, cite: string): void {
  openDeckPanel("sources");
  void openSource(session, deck, cite);
}

export function showSlide(session: SessionId, deck: string, slideId: string): void {
  useDecks.getState().select(session, deck, slideId);
  openDeckPanel("canvas");
}
