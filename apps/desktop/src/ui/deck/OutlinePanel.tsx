// The storyline: an editable table of the deck, one row per slide. The action title is the slide's
// title (`slide_update`), the key message is the slide's row of the storyline
// (`decks/<slug>/.present/outline.json`, written through `fs_write`), the layout switches the
// slide's layout, and the sources are what the slide cites (references and runs).
import { useState } from "react";
import { allLayouts, slideCites, slideTitle } from "../../deck/model";
import type { Deck } from "../../ipc";
import { setOutlineMessage, setSlideLayout, setSlideTitle } from "../../state/deckActions";
import { useDecks } from "../../state/decks";
import { useDeckContext } from "./context";
import { focusRunCite, openCite } from "./navigation";
import { NoDeck } from "./NoDeck";

/** Run citations of a slide's images, in reading order. */
export function slideRunCites(slide: Deck.Slide): Deck.RunCite[] {
  const out: Deck.RunCite[] = [];
  const add = (c: Deck.PlaceholderContent | Deck.ElementContent) => {
    if (c.type === "image" && c.run_cite && !out.some((x) => x.run_id === c.run_cite!.run_id && x.artifact === c.run_cite!.artifact)) out.push(c.run_cite);
  };
  for (const c of Object.values(slide.placeholders)) if (c) add(c);
  for (const e of slide.elements) add(e.content);
  return out;
}

export function OutlinePanel() {
  const ctx = useDeckContext();
  if (!ctx) return <NoDeck />;
  const { session, deck, d } = ctx;
  if (!d.deck || !d.info) return <div className="empty">Opening the deck…</div>;
  const layouts = allLayouts(d.info.template).filter((l) => l.usable);
  const readOnly = d.deck.mode === "code";
  const messageOf = (id: string) => d.outline.find((r) => r.slide_id === id)?.message ?? "";
  return (
    <div className="panel" data-testid="outline-view">
      <div className="outline">
        <table>
          <thead>
            <tr>
              <th className="num">#</th>
              <th>Action title</th>
              <th>Key message</th>
              <th>Layout</th>
              <th>Sources</th>
            </tr>
          </thead>
          <tbody>
            {d.deck.sections.map((section) => [
              section.title ? (
                <tr key={`sec:${section.title}`} className="section-row">
                  <td />
                  <td colSpan={4}>{section.title}</td>
                </tr>
              ) : null,
              ...section.slides.map((id) => {
                const v = d.slides[id];
                if (!v) return null;
                const title = slideTitle(v.slide);
                const locked = readOnly;
                const cites = slideCites(v.slide);
                const runs = slideRunCites(v.slide);
                return (
                  <tr key={id} className={d.selected === id ? "selected" : undefined} data-testid="outline-row" data-slide-id={id} onClick={() => useDecks.getState().select(session, deck, id)}>
                    <td className="num">{v.index + 1}</td>
                    <td>
                      <CellInput key={title} value={title} disabled={locked} label={`Title of slide ${v.index + 1}`} onCommit={(t) => void setSlideTitle(session, deck, id, t)} />
                    </td>
                    <td>
                      <CellInput key={messageOf(id)} value={messageOf(id)} disabled={readOnly} label={`Key message of slide ${v.index + 1}`} onCommit={(m) => void setOutlineMessage(session, deck, id, title, m)} />
                    </td>
                    <td>
                      <select aria-label={`Layout of slide ${v.index + 1}`} value={v.slide.layout} disabled={locked} onChange={(e) => void setSlideLayout(session, deck, id, e.target.value)}>
                        {layouts.some((l) => l.name === v.slide.layout) ? null : <option value={v.slide.layout}>{v.slide.layout}</option>}
                        {layouts.map((l) => (
                          <option key={l.name} value={l.name}>
                            {l.name}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td className="status">
                      {cites.map((c, i) => (
                        <button key={c} type="button" className="pill tag" title={c} onClick={() => openCite(session, deck, c)}>
                          [{i + 1}]
                        </button>
                      ))}
                      {runs.map((r) => (
                        <button key={`${r.run_id}:${r.artifact}`} type="button" className="pill tag" title={`${r.script} ${r.args.join(" ")}`} onClick={() => focusRunCite(session, r)}>
                          run {r.artifact.split("/").pop()}
                        </button>
                      ))}
                      {cites.length + runs.length === 0 ? <span className="muted small">none</span> : null}
                    </td>
                  </tr>
                );
              }),
            ])}
          </tbody>
        </table>
        {Object.keys(d.slides).length === 0 ? <div className="empty">No slides yet.</div> : null}
      </div>
    </div>
  );
}

function CellInput({ value, disabled, label, onCommit }: { value: string; disabled: boolean; label: string; onCommit: (text: string) => void }) {
  const [text, setText] = useState(value);
  return (
    <input
      type="text"
      aria-label={label}
      value={text}
      disabled={disabled}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => text !== value && onCommit(text)}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        else if (e.key === "Escape") {
          setText(value);
          requestAnimationFrame(() => (e.target as HTMLInputElement).blur());
        }
      }}
    />
  );
}
