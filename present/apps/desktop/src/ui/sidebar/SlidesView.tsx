// The slide rail: thumbnails drawn by the live preview (or the fidelity image when one is current),
// section headers, selection, drag to reorder, check and number badges, lock and hidden state.
import { useState } from "react";
import type { DragEvent, KeyboardEvent } from "react";
import { ContextMenu } from "@wb/ui";
import type { MenuItem } from "@wb/ui";
import { orderedSlideIds, slideTitle } from "../../deck/model";
import { projectFileUrl } from "../../ipc/files";
import type { ProjectInfo, SlideView } from "../../ipc";
import { createSlide, deleteSlide, duplicateSlide, moveSlideAfter, setSlideLocked, updateSlide } from "../../state/actions";
import { useDeck, useProjectDeck } from "../../state/deck";
import { useActiveProject, useProjectInfo } from "../../state/projects";
import { SlidePreview } from "../slide/SlidePreview";
import { useElementSize } from "../useElementSize";

/** Where a dragged slide lands relative to the hovered item: the id it will sit after (null = first). */
export function dropTarget(order: string[], dragged: string, over: string, before: boolean): string | null {
  const rest = order.filter((id) => id !== dragged);
  const i = rest.indexOf(over);
  if (i < 0) return over === dragged ? (order[order.indexOf(dragged) - 1] ?? null) : null;
  return before ? (rest[i - 1] ?? null) : over;
}

export function SlidesView() {
  const project = useActiveProject();
  const info = useProjectInfo(project);
  const d = useProjectDeck(project);
  const [ref, size] = useElementSize<HTMLDivElement>();
  const [dragging, setDragging] = useState<string | null>(null);
  const [drop, setDrop] = useState<{ id: string; before: boolean } | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; id: string } | null>(null);

  if (!project || !info || !d) return <div className="panel" />;
  const order = orderedSlideIds(d.deck);
  const thumbWidth = Math.max(60, size.width - 16 - 18 - 6 - 8 - 2);

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
    if (dragging && drop && drop.id !== dragging) void moveSlideAfter(project, dragging, dropTarget(order, dragging, drop.id, drop.before));
    setDragging(null);
    setDrop(null);
  };
  const onKeyDown = (e: KeyboardEvent, id: string) => {
    // Alt+arrows move the slide (keymap); plain arrows move the selection.
    if (e.altKey || e.metaKey || e.ctrlKey) return;
    const i = order.indexOf(id);
    const next = e.key === "ArrowDown" ? order[i + 1] : e.key === "ArrowUp" ? order[i - 1] : undefined;
    if (next) {
      e.preventDefault();
      useDeck.getState().select(project, next);
      requestAnimationFrame(() => document.querySelector<HTMLElement>(`[data-rail] [data-slide-id="${next}"]`)?.focus());
    } else if (e.key === "Delete") void deleteSlide(project, id);
  };

  const menuItems = (id: string): MenuItem[] => {
    const v = d.slides[id];
    if (!v) return [];
    const readOnly = d.deck.mode === "code";
    return [
      { label: "New slide after", disabled: readOnly, onSelect: () => void createSlide(project, v.slide.layout === "Title Slide" ? "Title and Content" : v.slide.layout, id) },
      { label: "Duplicate", disabled: readOnly, onSelect: () => void duplicateSlide(project, id) },
      { label: v.slide.locked ? "Unlock" : "Lock", onSelect: () => void setSlideLocked(project, id, !v.slide.locked) },
      { label: v.slide.hidden ? "Show slide" : "Hide slide", disabled: readOnly, onSelect: () => void updateSlide(project, { ...v.slide, hidden: !v.slide.hidden }) },
      { label: "Delete", danger: true, disabled: readOnly || order.length <= 1, onSelect: () => void deleteSlide(project, id) },
    ];
  };

  return (
    <div className="panel" data-testid="slides-view">
      <div className="rail" ref={ref} data-rail onDragEnd={() => (setDragging(null), setDrop(null))}>
        {d.deck.sections.map((section) => (
          <div key={section.title} className="rail-group">
            {d.deck.sections.length > 1 || section.title ? (
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
                  draggable={d.deck.mode !== "code"}
                  data-testid="rail-item"
                  data-slide-id={id}
                  onClick={() => useDeck.getState().select(project, id)}
                  onFocus={() => useDeck.getState().select(project, id)}
                  onKeyDown={(e) => onKeyDown(e, id)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    useDeck.getState().select(project, id);
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
                  <RailThumb view={v} info={info} width={thumbWidth} codeMode={d.deck.mode === "code"} />
                </div>
              );
            })}
          </div>
        ))}
      </div>
      {menu ? <ContextMenu x={menu.x} y={menu.y} items={menuItems(menu.id)} onClose={() => setMenu(null)} /> : null}
    </div>
  );
}

function RailThumb({ view, info, width, codeMode }: { view: SlideView; info: ProjectInfo; width: number; codeMode: boolean }) {
  const fidelity = view.thumb.kind === "fidelity" && !view.thumb.stale ? projectFileUrl(info, view.thumb.path) : null;
  const n = view.numbers;
  return (
    <div className="rail-thumb" data-thumb={fidelity ? "fidelity" : "preview"}>
      {fidelity ? <img src={fidelity} alt="" draggable={false} /> : <SlidePreview slide={view.slide} template={info.template} deck={info.deck} index={view.index} width={width} resolveAsset={(p) => projectFileUrl(info, p)} />}
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
