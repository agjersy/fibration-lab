// Speaker notes of the selected slide, saved through `slide_update` when the field loses focus.
import { useState } from "react";
import { plainText, slideTitle } from "../../deck/model";
import { setSlideNotes } from "../../state/deckActions";
import { selectedView } from "../../state/decks";
import { useDeckContext } from "./context";
import { NoDeck } from "./NoDeck";

function NotesEditor({ value, disabled, onSave }: { value: string; disabled: boolean; onSave: (text: string) => void }) {
  const [text, setText] = useState(value);
  return (
    <textarea
      className="grow"
      aria-label="Speaker notes"
      data-testid="notes-input"
      value={text}
      disabled={disabled}
      placeholder={disabled ? "" : "Notes for the speaker…"}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => text !== value && onSave(text)}
      style={{ width: "100%", minHeight: 80, resize: "vertical" }}
    />
  );
}

export function NotesPanel() {
  const ctx = useDeckContext();
  if (!ctx) return <NoDeck />;
  const view = selectedView(ctx.d);
  if (!view) return <div className="empty">Select a slide to see its notes.</div>;
  const value = plainText(view.slide.notes);
  const readOnly = ctx.d.deck?.mode === "code" || !!view.slide.locked;
  return (
    <div className="panel" data-testid="notes-panel">
      <div className="panel-toolbar">
        <span className="small muted truncate">Speaker notes · {slideTitle(view.slide) || `Slide ${view.index + 1}`}</span>
      </div>
      <div className="panel-body padded">
        <NotesEditor key={`${view.slide.id}:${value}`} value={value} disabled={readOnly} onSave={(t) => void setSlideNotes(ctx.session, ctx.deck, view.slide.id, t)} />
      </div>
    </div>
  );
}
