// The slide canvas: a large live preview of the selected slide with in-place text editing, the
// layout picker, the lock toggle and citation markers. In code mode it is read-only and shows the
// fidelity thumbnail.
import { useEffect, useRef } from "react";
import { allLayouts, newElementId } from "../../deck/model";
import type { LayoutInfo, ProjectId, Slide, TemplateDescription, Text } from "../../ipc";
import { projectFileUrl } from "../../ipc/files";
import { openSource, setSlideLayout, setSlideLocked, updateSlide } from "../../state/actions";
import { selectedView, useProjectDeck } from "../../state/deck";
import { useActiveProject, useProjectInfo } from "../../state/projects";
import { useUi } from "../../state/ui";
import { slideSize } from "../slide/geometry";
import { SlidePreview } from "../slide/SlidePreview";
import { useElementSize } from "../useElementSize";
import { showPanel } from "../workbench/controller";

/** The slide after an in-place edit of one text placeholder. Null when nothing would change. */
export function withEditedText(slide: Slide, key: string, text: Text): Slide | null {
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
  const project = useActiveProject();
  const info = useProjectInfo(project);
  const d = useProjectDeck(project);
  const view = selectedView(d);
  const [ref, size] = useElementSize<HTMLDivElement>();
  const pickerOpen = useUi((s) => s.layoutPickerOpen);

  if (!project || !info || !d) return <div className="canvas" />;
  if (!view) {
    return (
      <div className="canvas" data-testid="canvas">
        <div className="empty">No slide selected.</div>
      </div>
    );
  }
  const slide = view.slide;
  const codeMode = d.deck.mode === "code";
  const dims = slideSize(info.template, d.deck);
  const pad = 24;
  const width = Math.max(120, Math.floor(Math.min(size.width - 2 * pad, ((size.height - 2 * pad) * dims.cx) / dims.cy)));
  const fidelity = codeMode && view.thumb.kind !== "none" ? projectFileUrl(info, view.thumb.path) : null;

  const onEditText = (key: string, text: Text) => {
    const next = withEditedText(slide, key, text);
    if (next) void updateSlide(project, next);
  };
  const onCite = (cite: string) => {
    showPanel("sources");
    void openSource(project, cite);
  };

  return (
    <div className="canvas" data-testid="canvas" data-slide-id={slide.id} data-mode={d.deck.mode}>
      {codeMode ? (
        <div className="canvas-banner" data-testid="code-mode-banner">
          Code mode: the deck is built by deck/build.py. The canvas is read-only{view.thumb.stale ? "; the image may be out of date" : ""}.
        </div>
      ) : null}
      <div className="canvas-stage" ref={ref}>
        {size.width > 0 ? (
          <div className="canvas-slide" style={{ width }}>
            {fidelity ? (
              <img src={fidelity} alt={`Slide ${view.index + 1}`} style={{ width }} data-testid="canvas-fidelity" />
            ) : (
              <SlidePreview
                key={slide.id}
                slide={slide}
                template={info.template}
                deck={d.deck}
                index={view.index}
                width={width}
                resolveAsset={(p) => projectFileUrl(info, p)}
                interactive={!codeMode}
                {...(codeMode ? {} : { onEditText })}
                onCite={onCite}
              />
            )}
          </div>
        ) : null}
        {pickerOpen && !codeMode ? <LayoutPicker project={project} slide={slide} template={info.template} thumbs={Object.fromEntries(d.layoutThumbs.map((t) => [t.layout, projectFileUrl(info, t.png)]))} /> : null}
      </div>
      <div className="canvas-bar">
        <span className="muted small">Layout</span>
        <button type="button" disabled={codeMode} aria-haspopup="listbox" aria-expanded={pickerOpen} onClick={() => useUi.getState().setLayoutPickerOpen(!pickerOpen)} data-testid="layout-button">
          {slide.layout} ▾
        </button>
        <button type="button" className={slide.locked ? "" : "ghost"} aria-pressed={!!slide.locked} title={slide.locked ? "Locked: the agent may not change this slide. Click to unlock." : "Lock this slide against agent changes"} onClick={() => void setSlideLocked(project, slide.id, !slide.locked)} data-testid="lock-toggle">
          {slide.locked ? "🔒 Locked" : "🔓 Lock"}
        </button>
        {slide.kind === "generated" ? <span className="badge accent">generated</span> : null}
        <span className="spacer" />
        {view.checks.errors > 0 ? <span className="badge error">{view.checks.errors} error{view.checks.errors === 1 ? "" : "s"}</span> : null}
        {view.checks.warnings > 0 ? <span className="badge warning">{view.checks.warnings} warning{view.checks.warnings === 1 ? "" : "s"}</span> : null}
        <span className="muted small" data-testid="slide-position">
          Slide {view.index + 1} of {Object.keys(d.slides).length}
        </span>
      </div>
    </div>
  );
}

function LayoutSketch({ layout, template }: { layout: LayoutInfo; template: TemplateDescription }) {
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

function LayoutPicker({ project, slide, template, thumbs }: { project: ProjectId; slide: Slide; template: TemplateDescription; thumbs: Record<string, string | null> }) {
  const ref = useRef<HTMLDivElement>(null);
  const close = () => useUi.getState().setLayoutPickerOpen(false);
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element;
      if (ref.current && !ref.current.contains(t) && !t.closest('[data-testid="layout-button"]')) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    ref.current?.querySelector<HTMLElement>(".current")?.focus();
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, []);
  return (
    <div className="layout-picker" role="listbox" aria-label="Slide layout" ref={ref} data-testid="layout-picker">
      {allLayouts(template)
        .filter((l) => l.usable || l.name === slide.layout)
        .map((l) => (
          <button
            key={l.name}
            type="button"
            role="option"
            aria-selected={l.name === slide.layout}
            className={`layout-option${l.name === slide.layout ? " current" : ""}`}
            data-layout={l.name}
            onClick={() => {
              close();
              void setSlideLayout(project, slide.id, l.name);
            }}
          >
            {thumbs[l.name] ? <img src={thumbs[l.name]!} alt="" /> : <LayoutSketch layout={l} template={template} />}
            <span>
              <span className="name">{l.name}</span>
              {l.description ? <span className="desc"> — {l.description}</span> : null}
              <br />
              <span className="desc">{l.placeholders.filter((p) => p.accepts.length > 0 && p.type !== "ftr").map((p) => p.key).join(", ")}</span>
            </span>
          </button>
        ))}
    </div>
  );
}
