// The slide rail: thumbnails drawn by the live preview (or the fidelity render when it is current),
// section headers, selection, drag to reorder, check and number badges, lock and hidden state, and
// add / duplicate / delete.
import { useState } from "react";
import type { DragEvent, KeyboardEvent } from "react";
import { allLayouts, orderedSlideIds, slideTitle } from "../../deck/model";
import type { Deck } from "../../ipc";
import { createSlide, deleteSlide, duplicateSlide, moveSlideAfter, setSlideLocked, updateSlide } from "../../state/deckActions";
import { useDecks } from "../../state/decks";
import type { DeckState } from "../../state/decks";
import { ContextMenu } from "../common/ContextMenu";
import type { MenuItem } from "../common/ContextMenu";
import { useElementSize } from "../common/useElementSize";
import { useDeckContext } from "./context";
import { NoDeck } from "./NoDeck";
import { SlidePreview } from "./slide/SlidePreview";

/** Where a dragged slide lands relative to the hovered item: the id it will sit after (null = first). */
export function dropTarget(order: string[], dragged: string, over: string, before: boolean): string | null {
  const rest = order.filter((id) => id !== dragged);
  const i = rest.indexOf(over);
  if (i < 0) return over === dragged ? (order[order.indexOf(dragged) - 1] ?? null) : null;
  return before ? (rest[i - 1] ?? null) : over;
}

/** The layout a new slide gets: the selected slide's, except after a title slide; else the template's second layout. */
export function newSlideLayout(d: DeckState): string | null {
  const layouts = d.info ? allLayouts(d.info.template).filter((l) => l.usable) : [];
  const current = d.selected ? d.slides[d.selected]?.slide.layout : undefined;
  if (current && current !== "Title Slide" && layouts.some((l) => l.name === current)) return current;
  return (layouts.find((l) => l.name === "Title and Content") ?? layouts[1] ?? layouts[0])?.name ?? null;
}

export function SlidesPanel() {
  const ctx = useDeckContext();
  const [ref, size] = useElementSize<HTMLDivElement>();
  const [dragging, setDragging] = useState<string | null>(null);
  const [drop, setDrop] = useState<{ id: string; before: boolean } | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; id: string } | null>(null);

  const body = (() => {
    if (!ctx) return <NoDeck />;
    const { session, deck, d, resolve } = ctx;
    if (!d.deck || !d.info) return <div className="empty">Opening the deck…</div>;
    const doc = d.deck;
    const info = d.info;
    const order = orderedSlideIds(doc);
    const readOnly = doc.mode === "code";
    const thumbWidth = Math.max(60, size.width - 16 - 18 - 6 - 8 - 2);
    const select = (id: string) => useDecks.getState().select(session, deck, id);

    const onDragOver = (e: DragEvent, id: string) => {
      if (!dragging) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
      const before = e.clientY < r.top + r.height / 2;
      setDrop((cur) => (cur && cur.id === id && cur.before === before ? cur : { id, before }));
    };
    const onDrop = (e: DragEvent) => {
      e.preventDefault();
      if (dragging && drop && drop.id !== dragging) void moveSlideAfter(session, deck, dragging, dropTarget(order, dragging, drop.id, drop.before));
      setDragging(null);
      setDrop(null);
    };
    const onKeyDown = (e: KeyboardEvent, id: string) => {
      if (e.altKey || e.metaKey || e.ctrlKey) return;
      const i = order.indexOf(id);
      const next = e.key === "ArrowDown" ? order[i + 1] : e.key === "ArrowUp" ? order[i - 1] : undefined;
      if (next) {
        e.preventDefault();
        select(next);
        requestAnimationFrame(() => document.querySelector<HTMLElement>(`[data-rail] [data-slide-id="${next}"]`)?.focus());
      } else if (e.key === "Delete" && !readOnly) void deleteSlide(session, deck, id);
    };
    const menuItems = (id: string): MenuItem[] => {
      const v = d.slides[id];
      if (!v) return [];
      const layout = newSlideLayout(d);
      return [
        { label: "New slide after", disabled: readOnly || !layout, onSelect: () => layout && void createSlide(session, deck, layout, id) },
        { label: "Duplicate", disabled: readOnly, onSelect: () => void duplicateSlide(session, deck, id) },
        { label: v.slide.locked ? "Unlock" : "Lock", onSelect: () => void setSlideLocked(session, deck, id, !v.slide.locked) },
        { label: v.slide.hidden ? "Show slide" : "Hide slide", disabled: readOnly, onSelect: () => void updateSlide(session, deck, { ...v.slide, hidden: !v.slide.hidden }) },
        { label: "Delete", danger: true, disabled: readOnly, onSelect: () => void deleteSlide(session, deck, id) },
      ];
    };
    const layout = newSlideLayout(d);
    return (
      <>
        <div className="panel-toolbar">
          <span className="grow small muted truncate" title={doc.title}>
            {doc.title} · {order.length} slide{order.length === 1 ? "" : "s"}
          </span>
          <button type="button" className="small" disabled={readOnly || !layout} onClick={() => layout && void createSlide(session, deck, layout, d.selected ?? undefined)} title={layout ? `Add a slide (${layout}) after the selected one` : "The template has no usable layout"} data-testid="add-slide">
            + Slide
          </button>
          <button type="button" className="small" disabled={readOnly || !d.selected} onClick={() => d.selected && void duplicateSlide(session, deck, d.selected)} data-testid="duplicate-slide">
            Duplicate
          </button>
          <button type="button" className="small danger" disabled={readOnly || !d.selected} onClick={() => d.selected && void deleteSlide(session, deck, d.selected)} data-testid="delete-slide">
            Delete
          </button>
        </div>
        {order.length === 0 ? <div className="empty">No slides yet. Add one, or ask the agent in slides mode.</div> : null}
        {doc.sections.map((section) => (
          <div key={section.title} className="rail-group">
            {doc.sections.length > 1 || section.title ? (
              <div className="rail-section" data-testid="rail-section">
                {section.title}
              </div>
            ) : null}
            {section.slides.map((id) => {
              const v = d.slides[id];
              if (!v) return null;
              const cls = ["rail-item", d.selected === id ? "selected" : "", dragging === id ? "dragging" : "", v.slide.hidden ? "hidden-slide" : "", drop?.id === id && dragging !== id ? (drop.before ? "drop-before" : "drop-after") : ""].filter(Boolean).join(" ");
              return (
                <div
                  key={id}
                  className={cls}
                  role="option"
                  aria-selected={d.selected === id}
                  aria-label={`Slide ${v.index + 1}: ${slideTitle(v.slide)}`}
                  tabIndex={0}
                  draggable={!readOnly}
                  data-testid="rail-item"
                  data-slide-id={id}
                  onClick={() => select(id)}
                  onFocus={() => select(id)}
                  onKeyDown={(e) => onKeyDown(e, id)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    select(id);
                    setMenu({ x: e.clientX, y: e.clientY, id });
                  }}
                  onDragStart={(e) => {
                    e.dataTransfer.effectAllowed = "move";
                    e.dataTransfer.setData("text/plain", id);
                    setDragging(id);
                  }}
                  onDragOver={(e) => onDragOver(e, id)}
                  onDrop={onDrop}
                >
                  <div className="rail-gutter">
                    <span>{v.index + 1}</span>
                    {v.slide.locked ? (
                      <span className="glyph" title="Locked: the agent may not change this slide" data-testid="lock-icon">
                        🔒
                      </span>
                    ) : null}
                    {v.slide.hidden ? (
                      <span className="glyph" title="Hidden slide">
                        ⦸
                      </span>
                    ) : null}
                  </div>
                  <RailThumb view={v} template={info.template} doc={doc} width={thumbWidth} codeMode={readOnly} resolve={resolve} />
                </div>
              );
            })}
          </div>
        ))}
        {menu ? <ContextMenu x={menu.x} y={menu.y} items={menuItems(menu.id)} onClose={() => setMenu(null)} /> : null}
      </>
    );
  })();

  return (
    <div className="panel" data-testid="slides-view">
      <div
        className="rail"
        ref={ref}
        data-rail
        onDragEnd={() => {
          setDragging(null);
          setDrop(null);
        }}
      >
        {body}
      </div>
    </div>
  );
}

function RailThumb({
  view,
  template,
  doc,
  width,
  codeMode,
  resolve,
}: {
  view: Deck.SlideView;
  template: Deck.TemplateDescription;
  doc: Deck.Deck;
  width: number;
  codeMode: boolean;
  resolve: (p: string) => string | null;
}) {
  const fidelity = view.thumb.kind === "fidelity" && !view.thumb.stale ? resolve(view.thumb.path ?? "") : null;
  const n = view.numbers;
  return (
    <div className="rail-thumb" data-thumb={fidelity ? "fidelity" : "preview"}>
      {fidelity ? <img src={fidelity} alt="" draggable={false} /> : <SlidePreview slide={view.slide} template={template} deck={doc} index={view.index} width={width} resolveAsset={resolve} />}
      {fidelity ? <span className="fidelity-mark" title="Rendered by the fidelity engine" /> : null}
      <div className="rail-badges">
        {view.slide.kind === "generated" ? <span className="pill tag">gen</span> : null}
        {codeMode ? <span className="pill tag">code</span> : null}
        {n.red > 0 ? (
          <span className="pill num-red" title={`${n.red} number(s) differ from the cited source`} data-testid="badge-red">
            {n.red}
          </span>
        ) : null}
        {n.amber > 0 ? (
          <span className="pill num-amber" title={`${n.amber} number(s) match only after rounding`}>
            {n.amber}
          </span>
        ) : null}
        {n.grey > 0 ? (
          <span className="pill num-grey" title={`${n.grey} number(s) without a citation`} data-testid="badge-grey">
            {n.grey}
          </span>
        ) : null}
        {view.checks.errors > 0 ? (
          <span className="pill err" title={`${view.checks.errors} checker error(s)`} data-testid="badge-errors">
            {view.checks.errors}
          </span>
        ) : null}
        {view.checks.warnings > 0 ? (
          <span className="pill warn" title={`${view.checks.warnings} checker warning(s)`} data-testid="badge-warnings">
            {view.checks.warnings}
          </span>
        ) : null}
      </div>
    </div>
  );
}
