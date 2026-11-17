// The outline: an editable table of the deck (title, layout, lock, status). Editing a title issues
// `slide_update` with the slide document changed in its title placeholder only.
import { useState } from "react";
import { allLayouts, slideTitle } from "../../deck/model";
import type { SlideView } from "../../ipc";
import { setSlideLayout, setSlideLocked, setSlideTitle } from "../../state/actions";
import { useDeck, useProjectDeck } from "../../state/deck";
import { useActiveProject, useProjectInfo } from "../../state/projects";

export function OutlineView() {
  const project = useActiveProject();
  const info = useProjectInfo(project);
  const d = useProjectDeck(project);
  if (!project || !info || !d) return <div className="panel" />;
  const layouts = allLayouts(info.template).filter((l) => l.usable);
  const readOnly = d.deck.mode === "code";
  return (
    <div className="panel" data-testid="outline-view">
      <div className="outline">
        <table>
          <thead>
            <tr>
              <th className="num">#</th>
              <th>Title</th>
              <th>Layout</th>
              <th title="Locked slides are not changed by the agent">Lock</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {d.deck.sections.map((section) => [
              <tr key={`sec:${section.title}`} className="section-row">
                <td />
                <td colSpan={4}>{section.title}</td>
              </tr>,
              ...section.slides.map((id) => {
                const v = d.slides[id];
                if (!v) return null;
                return (
                  <tr key={id} className={d.selected === id ? "selected" : undefined} data-testid="outline-row" data-slide-id={id} onClick={() => useDeck.getState().select(project, id)}>
                    <td className="num">{v.index + 1}</td>
                    <td>
                      <TitleInput key={slideTitle(v.slide)} value={slideTitle(v.slide)} disabled={readOnly} label={`Title of slide ${v.index + 1}`} onCommit={(t) => void setSlideTitle(project, id, t)} />
                    </td>
                    <td>
                      <select aria-label={`Layout of slide ${v.index + 1}`} value={v.slide.layout} disabled={readOnly} onChange={(e) => void setSlideLayout(project, id, e.target.value)}>
                        {layouts.some((l) => l.name === v.slide.layout) ? null : <option value={v.slide.layout}>{v.slide.layout}</option>}
                        {layouts.map((l) => (
                          <option key={l.name} value={l.name}>
                            {l.name}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td>
                      <input type="checkbox" aria-label={`Lock slide ${v.index + 1}`} checked={!!v.slide.locked} onChange={(e) => void setSlideLocked(project, id, e.target.checked)} />
                    </td>
                    <td className="status">
                      <Status view={v} />
                    </td>
                  </tr>
                );
              }),
            ])}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function TitleInput({ value, disabled, label, onCommit }: { value: string; disabled: boolean; label: string; onCommit: (title: string) => void }) {
  const [text, setText] = useState(value);
  const commit = () => {
    if (text !== value) onCommit(text);
  };
  return (
    <input
      type="text"
      aria-label={label}
      value={text}
      disabled={disabled}
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
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

function Status({ view }: { view: SlideView }) {
  const { checks, numbers, slide } = view;
  const clean = checks.errors + checks.warnings + numbers.red + numbers.amber + numbers.grey === 0;
  return (
    <>
      {checks.errors > 0 ? <span className="pill err">{checks.errors}</span> : null}
      {checks.warnings > 0 ? <span className="pill warn">{checks.warnings}</span> : null}
      {numbers.red > 0 ? <span className="pill num-red">{numbers.red}</span> : null}
      {numbers.amber > 0 ? <span className="pill num-amber">{numbers.amber}</span> : null}
      {numbers.grey > 0 ? <span className="pill num-grey">{numbers.grey}</span> : null}
      {clean ? <span className="muted small">ok</span> : null}
      {slide.hidden ? <span className="muted small"> hidden</span> : null}
    </>
  );
}
