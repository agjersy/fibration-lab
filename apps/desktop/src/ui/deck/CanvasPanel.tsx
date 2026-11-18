// The slide canvas: a large live preview of the selected slide with in-place text editing
// (`slide_update` with the expected hash), the layout picker, the lock toggle, citation markers
// (reference citations open the source passage, run citations open the run) and stale markers. In
// code mode it is read-only and shows the fidelity render.
import { useEffect, useRef, useState } from "react";
import { allLayouts, newElementId } from "../../deck/model";
import type { Deck } from "../../ipc";
import { setSlideLayout, setSlideLocked, updateSlide } from "../../state/deckActions";
import { selectedView, staleElements } from "../../state/decks";
import { useElementSize } from "../common/useElementSize";
import { useDeckContext } from "./context";
import { focusRunCite, openCite } from "./navigation";
import { NoDeck } from "./NoDeck";
import { slideSize } from "./slide/geometry";
import { SlidePreview } from "./slide/SlidePreview";

/** The slide after an in-place edit of one text placeholder. Null when nothing would change. */
export function withEditedText(slide: Deck.Slide, key: string, text: Deck.Text): Deck.Slide | null {
  const cur = slide.placeholders[key];
  const empty = text.paragraphs.every((p) => p.runs.every((r) => r.t === ""));
  if (!cur || cur.type === "empty") {
    if (empty) return null;
    return { ...slide, placeholders: { ...slide.placeholders, [key]: { type: "text", id: newElementId(slide), text } } };
  }
  if (cur.type !== "text") return null;
  return { ...slide, placeholders: { ...slide.placeholders, [key]: { ...cur, text } } };
}

export function CanvasPanel() {
  const ctx = useDeckContext();
  const [ref, size] = useElementSize<HTMLDivElement>();
  const [pickerOpen, setPickerOpen] = useState(false);
  const view = selectedView(ctx?.d ?? null);
  const doc = ctx?.d.deck ?? null;
  const info = ctx?.d.info ?? null;

  let stage: React.ReactNode = null;
  let bar: React.ReactNode = null;
  let banner: React.ReactNode = null;
  if (!ctx) stage = <NoDeck />;
  else if (!doc || !info) stage = <div className="empty">Opening the deck…</div>;
  else if (!view) stage = <div className="empty">No slide selected.</div>;
  else {
    const { session, deck, d, resolve } = ctx;
    const slide = view.slide;
    const codeMode = doc.mode === "code";
    const dims = slideSize(info.template, doc);
    const pad = 24;
    const width = Math.max(120, Math.floor(Math.min(size.width - 2 * pad, ((size.height - 2 * pad) * dims.cx) / dims.cy)));
    const fidelity = codeMode && view.thumb.kind !== "none" ? resolve(view.thumb.path) : null;
    const onEditText = (key: string, text: Deck.Text) => {
      const next = withEditedText(slide, key, text);
      if (next) void updateSlide(session, deck, next);
    };
    if (codeMode) {
      banner = (
        <div className="canvas-banner" data-testid="code-mode-banner">
          Code mode: the deck is built by deck/build.py. The canvas is read-only{view.thumb.stale ? "; the image may be out of date" : ""}.
        </div>
      );
    }
    stage = (
      <>
        {size.width > 0 ? (
          <div className="canvas-slide" style={{ width }}>
            {fidelity ? (
              <img src={fidelity} alt={`Slide ${view.index + 1}`} style={{ width }} data-testid="canvas-fidelity" />
            ) : (
              <SlidePreview
                key={slide.id}
                slide={slide}
                template={info.template}
                deck={doc}
                index={view.index}
                width={width}
                resolveAsset={resolve}
                interactive={!codeMode && !slide.locked}
                {...(codeMode || slide.locked ? {} : { onEditText })}
                onCite={(cite) => openCite(session, deck, cite)}
                onRunCite={(cite) => focusRunCite(session, cite)}
                stale={staleElements(d, slide.id)}
              />
            )}
          </div>
        ) : null}
        {pickerOpen && !codeMode ? (
          <LayoutPicker
            slide={slide}
            template={info.template}
            thumbs={Object.fromEntries(d.layoutThumbs.map((t) => [t.layout, resolve(t.png)]))}
            onPick={(name) => {
              setPickerOpen(false);
              void setSlideLayout(session, deck, slide.id, name);
            }}
            onClose={() => setPickerOpen(false)}
          />
        ) : null}
      </>
    );
    const stale = staleElements(d, slide.id).size;
    bar = (
      <div className="canvas-bar">
        <span className="muted small">Layout</span>
        <button type="button" disabled={codeMode || !!slide.locked} aria-haspopup="listbox" aria-expanded={pickerOpen} onClick={() => setPickerOpen(!pickerOpen)} data-testid="layout-button">
          {slide.layout} ▾
        </button>
        <button
          type="button"
          className={slide.locked ? "" : "ghost"}
          aria-pressed={!!slide.locked}
          title={slide.locked ? "Locked: neither the agent nor an edit here changes this slide. Click to unlock." : "Lock this slide against changes"}
          onClick={() => void setSlideLocked(session, deck, slide.id, !slide.locked)}
          data-testid="lock-toggle"
        >
          {slide.locked ? "🔒 Locked" : "🔓 Lock"}
        </button>
        {slide.kind === "generated" ? <span className="badge accent">generated</span> : null}
        <span className="spacer" />
        {stale > 0 ? (
          <span className="badge warning" title="A figure on this slide comes from a run whose code has changed since" data-testid="stale-badge">
            {stale} stale
          </span>
        ) : null}
        {view.checks.errors > 0 ? <span className="badge error">{view.checks.errors} error{view.checks.errors === 1 ? "" : "s"}</span> : null}
        {view.checks.warnings > 0 ? <span className="badge warning">{view.checks.warnings} warning{view.checks.warnings === 1 ? "" : "s"}</span> : null}
        <span className="muted small" data-testid="slide-position">
          Slide {view.index + 1} of {Object.keys(d.slides).length}
        </span>
      </div>
    );
  }

  return (
    <div className="canvas" data-testid="canvas" data-slide-id={view?.slide.id} data-mode={doc?.mode}>
      {banner}
      <div className="canvas-stage" ref={ref}>
        {stage}
      </div>
      {bar}
    </div>
  );
}

function LayoutSketch({ layout, template }: { layout: Deck.LayoutInfo; template: Deck.TemplateDescription }) {
  const { cx, cy } = template.slide_size;
  return (
    <span className="layout-sketch" aria-hidden="true">
      {layout.placeholders
        .filter((p) => p.type !== "dt" && p.type !== "ftr" && p.type !== "sldNum")
        .map((p) => (
          <i key={p.key} style={{ left: `${(p.x / cx) * 100}%`, top: `${(p.y / cy) * 100}%`, width: `${(p.cx / cx) * 100}%`, height: `${(p.cy / cy) * 100}%` }} />
        ))}
    </span>
  );
}

function LayoutPicker({
  slide,
  template,
  thumbs,
  onPick,
  onClose,
}: {
  slide: Deck.Slide;
  template: Deck.TemplateDescription;
  thumbs: Record<string, string | null>;
  onPick: (layout: string) => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element;
      if (ref.current && !ref.current.contains(t) && !t.closest('[data-testid="layout-button"]')) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    ref.current?.querySelector<HTMLElement>(".current")?.focus();
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);
  return (
    <div className="layout-picker" role="listbox" aria-label="Slide layout" ref={ref} data-testid="layout-picker">
      {allLayouts(template)
        .filter((l) => l.usable || l.name === slide.layout)
        .map((l) => (
          <button key={l.name} type="button" role="option" aria-selected={l.name === slide.layout} className={`layout-option${l.name === slide.layout ? " current" : ""}`} data-layout={l.name} onClick={() => onPick(l.name)}>
            {thumbs[l.name] ? <img src={thumbs[l.name]!} alt="" /> : <LayoutSketch layout={l} template={template} />}
            <span>
              <span className="name">{l.name}</span>
              {l.description ? <span className="desc"> — {l.description}</span> : null}
              <br />
              <span className="desc">
                {l.placeholders
                  .filter((p) => p.accepts.length > 0 && p.type !== "ftr")
                  .map((p) => p.key)
                  .join(", ")}
              </span>
            </span>
          </button>
        ))}
    </div>
  );
}
