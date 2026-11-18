// The live preview (IPC.md section 5): a slide drawn from the deck document and the template
// description. Layout placeholders sit at their EMU geometry scaled to the pixel width; text takes
// the level styles of its placeholder; charts and diagrams are SVG. Pure: no stores, no backend.
import { Suspense, lazy, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ClipboardEvent, KeyboardEvent, MouseEvent, ReactNode } from "react";
import { findLayout, isFurniture, isTitleType, slideCites } from "../../../deck/model";
import type { Deck, ElementContent, EqCite, FreeElement, LevelStyle, PlaceholderContent, PlaceholderInfo, RunCite, Slide, TableEl, TemplateDescription, Text, Theme } from "../../../deck/types";
import { ChartView } from "./ChartView";
import { DiagramView } from "./DiagramView";
import { emuRect, ptToPx, scaleFor, slideHeight, slideSize } from "./geometry";
import type { Rect } from "./geometry";
import type { VerificationState } from "../../../ipc";
import { canonicalText, domToText, mergeEdited, paragraphElement, restyleParagraph, shiftLevel, splitParagraph, textToHtml } from "./textModel";
import type { TextStyleContext } from "./textModel";
import { contrastOn, defaultAlign, fontStack, mix, resolveColor } from "./theme";

export interface SlidePreviewProps {
  slide: Slide;
  template: TemplateDescription;
  /** Pixel width of the slide; the height follows from the slide size. */
  width: number;
  /** For the footer, the slide number and a deck-level slide size. */
  deck?: Deck | null;
  /** 0-based position in the deck, for the slide number. */
  index?: number;
  /** Resolves an asset path (`assets/<sha>.png`) to a loadable URL. */
  resolveAsset?: (path: string) => string | null;
  /** Canvas mode: text placeholders are editable in place and empty ones show a prompt. */
  interactive?: boolean;
  /** A text placeholder was edited (key of the placeholder, new text). */
  onEditText?: (key: string, text: Text) => void;
  /** A citation marker was clicked. */
  onCite?: (cite: string) => void;
  /** An image's run citation badge was clicked (the figure came from an analysis run). */
  onRunCite?: (cite: RunCite, elementId: string) => void;
  /** Elements the checker reports as stale (their run's code has changed since). */
  stale?: ReadonlySet<string>;
  /** An equation was clicked (open it in the Equations panel at its function). */
  onEquation?: (cite: EqCite, elementId: string) => void;
  /** Verification state of the slide's claims (elements that cite the analysis), by element id. */
  verification?: ReadonlyMap<string, VerificationState>;
  /** A stale element's Refresh was clicked (re-run what changed and propose the update). */
  onRefresh?: (elementId: string) => void;
}

const Katex = lazy(() => import("../../panels/Katex"));

const VERIFY_MARK: Record<VerificationState, string> = { unverified: "?", machine_checked: "⚙", human_verified: "✓", flagged: "⚑" };
const VERIFY_TITLE: Record<VerificationState, string> = {
  unverified: "Not verified by you yet",
  machine_checked: "Checked by a tool or the agent, not by you",
  human_verified: "Verified by you",
  flagged: "Flagged",
};

const EMPTY_TEXT: Text = { paragraphs: [{ runs: [{ t: "" }] }] };
const FREE_LEVELS = (theme: Theme): LevelStyle[] => [0, 1, 2, 3, 4].map((level) => ({ level, font: theme.fonts.minor, size_pt: Math.max(10, 18 - level * 2), bold: false, color: "dk1", bullet: level > 0, space_before_pt: 4, line_spacing: 1, mar_l: level * 342900 }));

/** Steps the autofit factor down by 2.5% until the content fits, with a floor of 25%. */
export function fitFactor(contentHeight: (fit: number) => number, available: number): number {
  let fit = 1;
  while (fit > 0.25 && contentHeight(fit) > available + 0.5) fit = Math.max(0.25, Math.round((fit - 0.025) * 1000) / 1000);
  return fit;
}

function applyFit(box: HTMLElement | null, inner: HTMLElement | null, shrink: boolean): void {
  if (!box || !inner) return;
  box.style.setProperty("--fit", "1");
  if (!shrink) return;
  const cs = getComputedStyle(box);
  const available = box.clientHeight - (parseFloat(cs.paddingTop) || 0) - (parseFloat(cs.paddingBottom) || 0);
  if (available <= 0) return;
  fitFactor((fit) => {
    box.style.setProperty("--fit", String(fit));
    return inner.offsetHeight;
  }, available);
}

const NO_STALE: ReadonlySet<string> = new Set();

export function SlidePreview({ slide, template, width, deck, index, resolveAsset, interactive = false, onEditText, onCite, onRunCite, stale = NO_STALE, onEquation, verification, onRefresh }: SlidePreviewProps) {
  const size = slideSize(template, deck);
  const scale = scaleFor(size, width);
  const height = slideHeight(size, width);
  const theme = template.theme;
  const layout = findLayout(template, slide.layout);
  const citeNumbers = useMemo(() => new Map(slideCites(slide).map((c, i) => [c, i + 1])), [slide]);
  const shared: Shared = { theme, scale, citeNumbers, resolveAsset, interactive, onCite, onRunCite, stale, onEquation };

  const frame: CSSProperties = { width, height, background: resolveColor(theme, "lt1"), color: resolveColor(theme, "dk1"), fontFamily: fontStack(theme.fonts.minor, theme) };
  const footer = deck?.footer;
  const footerText = [deck?.classification, footer?.text].filter(Boolean).join(" · ");

  return (
    <div className={`slide-preview${interactive ? " interactive" : ""}`} style={frame} data-slide={slide.id} data-layout={slide.layout} data-testid={interactive ? "slide-canvas" : undefined}>
      {!layout ? <div className="sp-missing" style={{ fontSize: ptToPx(14, scale) }}>Layout “{slide.layout}” is not in the template</div> : null}
      {layout?.placeholders.map((ph) => {
        const rect = emuRect(ph, scale);
        if (isFurniture(ph)) {
          const text = ph.type === "ftr" ? footerText : ph.type === "sldNum" ? (footer?.slide_numbers && index !== undefined ? String(index + 1) : "") : footer?.date ? new Date().toLocaleDateString() : "";
          if (!text) return null;
          return <TextBox key={ph.key} ph={ph} rect={rect} text={{ paragraphs: [{ runs: [{ t: text }] }] }} shared={{ ...shared, interactive: false }} furniture />;
        }
        const content = slide.placeholders[ph.key];
        if (!content || content.type === "empty") {
          if (!interactive) return null;
          const canText = ph.accepts.includes("text");
          return (
            <TextBox
              key={ph.key}
              ph={ph}
              rect={rect}
              text={EMPTY_TEXT}
              shared={shared}
              prompt={canText ? (isTitleType(ph) ? "Click to add a title" : "Click to add text") : `Empty ${ph.accepts.join(" / ")} placeholder`}
              onCommit={canText && onEditText ? (t) => onEditText(ph.key, t) : undefined}
            />
          );
        }
        if (content.type === "text") return <TextBox key={ph.key} ph={ph} rect={rect} text={content.text} shared={shared} elementId={content.id} onCommit={onEditText ? (t) => onEditText(ph.key, t) : undefined} />;
        return (
          <div key={ph.key} className="sp-ph sp-object" data-ph={ph.key} data-element={content.id} data-type={content.type} style={{ ...rect }}>
            <ContentView content={content} rect={rect} shared={shared} levels={ph.levels} />
          </div>
        );
      })}
      {slide.elements.map((el) => (
        <FreeElementView key={el.id} element={el} shared={shared} />
      ))}
      {verification && verification.size > 0
        ? [...verification.entries()].map(([id, state]) => {
            const box = elementBox(slide, layout, id, scale);
            if (!box) return null;
            return (
              <span
                key={`v-${id}`}
                className={`sp-verify ${state}`}
                data-testid="slide-verify-badge"
                data-element={id}
                data-state={state}
                title={VERIFY_TITLE[state]}
                style={{ left: box.left + box.width, top: box.top, fontSize: Math.max(8, ptToPx(9, scale)) }}
              >
                {VERIFY_MARK[state]}
              </span>
            );
          })
        : null}
      {onRefresh
        ? [...stale].map((id) => {
            const box = elementBox(slide, layout, id, scale);
            if (!box) return null;
            return (
              <button
                key={`r-${id}`}
                type="button"
                className="sp-refresh"
                data-testid="element-refresh"
                data-element={id}
                title="The source changed: re-run what changed and propose the updated element"
                style={{ left: box.left, top: box.top, fontSize: Math.max(8, ptToPx(9, scale)) }}
                onClick={(e) => {
                  e.stopPropagation();
                  onRefresh(id);
                }}
              >
                ↻ Refresh
              </button>
            );
          })
        : null}
      {slide.kind === "generated" ? (
        <div className="sp-generated" style={{ fontSize: Math.max(8, ptToPx(10, scale)) }} title="Drawn by a script (generated slide)">
          generated
        </div>
      ) : null}
    </div>
  );
}

/** The pixel box of an element (placeholder content or free element) by id. */
function elementBox(slide: Slide, layout: ReturnType<typeof findLayout>, id: string, scale: number): Rect | null {
  const key = Object.keys(slide.placeholders).find((k) => {
    const c = slide.placeholders[k];
    return c && c.type !== "empty" && c.id === id;
  });
  const ph = key && layout ? layout.placeholders.find((p) => p.key === key) : undefined;
  if (ph) return emuRect(ph, scale);
  const free = slide.elements.find((e) => e.id === id);
  return free ? emuRect(free, scale) : null;
}

interface Shared {
  theme: Theme;
  scale: number;
  citeNumbers: Map<string, number>;
  resolveAsset?: (path: string) => string | null;
  interactive: boolean;
  onCite?: (cite: string) => void;
  onRunCite?: (cite: RunCite, elementId: string) => void;
  stale: ReadonlySet<string>;
  onEquation?: (cite: EqCite, elementId: string) => void;
}

function anchorJustify(anchor: string): CSSProperties["justifyContent"] {
  return anchor === "ctr" ? "center" : anchor === "b" ? "flex-end" : "flex-start";
}

interface TextBoxProps {
  ph: Pick<PlaceholderInfo, "key" | "type" | "autofit" | "anchor" | "levels" | "insets">;
  rect: Rect;
  text: Text;
  shared: Shared;
  elementId?: string;
  prompt?: string;
  furniture?: boolean;
  onCommit?: (text: Text) => void;
  style?: CSSProperties;
}

function TextBox({ ph, rect, text, shared, elementId, prompt, furniture, onCommit, style }: TextBoxProps) {
  const { theme, scale } = shared;
  const boxRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const [editing, setEditing] = useState(false);
  const caret = useRef<{ x: number; y: number } | null>(null);
  const cancelled = useRef(false);
  const editable = shared.interactive && !!onCommit;
  const ctx: TextStyleContext = useMemo(
    () => ({ theme, levels: ph.levels, scale, defaultAlign: defaultAlign(ph), citeNumbers: shared.citeNumbers, markers: true }),
    // `ph` is stable per layout placeholder; its type and levels are what the style depends on.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [theme, ph.levels, ph.type, scale, shared.citeNumbers],
  );
  const html = useMemo(() => textToHtml(text, ctx), [text, ctx]);
  const shrink = ph.autofit === "shrink";

  useLayoutEffect(() => {
    if (!editing) applyFit(boxRef.current, innerRef.current, shrink);
  }, [html, rect.width, rect.height, shrink, editing]);

  // Entering edit mode: fill the editing root once (React leaves its children alone), focus, place the caret.
  useLayoutEffect(() => {
    const root = innerRef.current;
    if (!editing || !root) return;
    cancelled.current = false;
    root.innerHTML = textToHtml(text, { ...ctx, markers: false });
    applyFit(boxRef.current, root, shrink);
    root.focus();
    const sel = window.getSelection();
    if (!sel) return;
    let range: Range | null = null;
    const at = caret.current;
    const doc = document as Document & { caretRangeFromPoint?: (x: number, y: number) => Range | null };
    if (at && doc.caretRangeFromPoint) range = doc.caretRangeFromPoint(at.x, at.y);
    if (!range || !root.contains(range.startContainer)) {
      range = document.createRange();
      range.selectNodeContents(root);
      range.collapse(false);
    }
    sel.removeAllRanges();
    sel.addRange(range);
    // Only when edit mode starts: later renders must not reset the DOM the user is typing in.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editing]);

  const boxStyle: CSSProperties = {
    ...rect,
    padding: `${ph.insets.t * scale}px ${ph.insets.r * scale}px ${ph.insets.b * scale}px ${ph.insets.l * scale}px`,
    justifyContent: anchorJustify(ph.anchor),
    ...style,
  };
  const cls = `sp-ph sp-textbox${editable ? " editable" : ""}${editing ? " editing" : ""}${furniture ? " furniture" : ""}`;
  const refit = () => applyFit(boxRef.current, innerRef.current, shrink);

  const commit = () => {
    const root = innerRef.current;
    setEditing(false);
    if (!root || cancelled.current || !onCommit) return;
    const edited = mergeEdited(text, domToText(root));
    if (canonicalText(edited) !== canonicalText(text)) onCommit(edited);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const root = innerRef.current;
    if (!root) return;
    const sel = window.getSelection();
    const mod = e.metaKey || e.ctrlKey;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cancelled.current = true;
      root.blur();
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (!sel || sel.rangeCount === 0) return;
      const range = sel.getRangeAt(0);
      if (!range.collapsed) range.deleteContents();
      const next = splitParagraph(root, range);
      if (!next) return;
      const prev = next.previousElementSibling as HTMLElement | null;
      if (prev) restyleParagraph(prev, ctx);
      restyleParagraph(next, ctx);
      const r = document.createRange();
      r.setStart(next, 0);
      r.collapse(true);
      sel.removeAllRanges();
      sel.addRange(r);
      refit();
    } else if (e.key === "Tab") {
      e.preventDefault();
      if (!sel || sel.rangeCount === 0) return;
      const range = sel.getRangeAt(0);
      const first = paragraphElement(range.startContainer, root);
      const last = paragraphElement(range.endContainer, root) ?? first;
      for (let p = first; p; p = p.nextElementSibling as HTMLElement | null) {
        shiftLevel(p, e.shiftKey ? -1 : 1, ctx);
        if (p === last) break;
      }
      refit();
    } else if (mod && !e.altKey && ["b", "i", "u"].includes(e.key.toLowerCase())) {
      e.preventDefault();
      e.stopPropagation();
      document.execCommand(e.key.toLowerCase() === "b" ? "bold" : e.key.toLowerCase() === "i" ? "italic" : "underline");
    }
  };

  const onPaste = (e: ClipboardEvent<HTMLDivElement>) => {
    // Plain text only: formatting, images and markup from the clipboard never enter the model.
    e.preventDefault();
    const root = innerRef.current;
    const lines = e.clipboardData.getData("text/plain").replace(/\r/g, "").split("\n");
    lines.forEach((line, i) => {
      const sel = window.getSelection();
      if (i > 0 && root && sel && sel.rangeCount) {
        const range = sel.getRangeAt(0);
        if (!range.collapsed) range.deleteContents();
        const next = splitParagraph(root, range);
        if (next) {
          restyleParagraph(next, ctx);
          const r = document.createRange();
          r.setStart(next, 0);
          r.collapse(true);
          sel.removeAllRanges();
          sel.addRange(r);
        }
      }
      if (line) document.execCommand("insertText", false, line);
    });
    refit();
  };

  const onClick = (e: MouseEvent<HTMLDivElement>) => {
    const marker = (e.target as Element).closest?.(".sp-cite");
    if (marker) {
      e.stopPropagation();
      const cite = marker.getAttribute("data-marker");
      if (cite) shared.onCite?.(cite);
      return;
    }
    if (editable && !editing) {
      caret.current = { x: e.clientX, y: e.clientY };
      setEditing(true);
    }
  };

  const isEmpty = text.paragraphs.every((p) => p.runs.every((r) => r.t === ""));
  return (
    <div ref={boxRef} className={cls} style={boxStyle} data-ph={ph.key} data-element={elementId} data-type="text" onClick={onClick}>
      {editing ? (
        <div
          key="edit"
          ref={innerRef}
          className="sp-text"
          contentEditable
          suppressContentEditableWarning
          spellCheck={false}
          role="textbox"
          aria-multiline="true"
          aria-label={`Edit ${ph.key}`}
          data-slide-editor={ph.key}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          onInput={refit}
          onBlur={commit}
          onDrop={(e) => e.preventDefault()}
        />
      ) : isEmpty && prompt ? (
        <div key="prompt" ref={innerRef} className="sp-text sp-prompt" style={{ fontSize: Math.min(ptToPx(ph.levels[0]?.size_pt ?? 18, scale), rect.height * 0.5), fontFamily: fontStack(ph.levels[0]?.font ?? "", theme), textAlign: defaultAlign(ph) }}>
          {prompt}
        </div>
      ) : (
        <div key="view" ref={innerRef} className="sp-text" dangerouslySetInnerHTML={{ __html: html }} />
      )}
    </div>
  );
}

function ContentView({ content, rect, shared, levels }: { content: PlaceholderContent | ElementContent; rect: Rect; shared: Shared; levels: LevelStyle[] }): ReactNode {
  const { theme, scale } = shared;
  // Object text is sized from the slide, not the box: 12 pt for charts and diagrams at any box size.
  const fontPx = ptToPx(12, scale);
  switch (content.type) {
    case "chart":
      return <ChartView chart={content.chart} theme={theme} width={rect.width} height={rect.height} fontPx={fontPx} />;
    case "diagram":
      return <DiagramView diagram={content.diagram} theme={theme} width={rect.width} height={rect.height} fontPx={ptToPx(14, scale)} />;
    case "table":
      return <TableView table={content.table} rect={rect} shared={shared} />;
    case "image":
      return <ImageView content={content} rect={rect} shared={shared} />;
    case "equation":
      return <EquationView id={content.id} cite={content.eq_cite} acknowledged={content.acknowledged === true} shared={shared} />;
    case "text":
      return <TextBox ph={{ key: content.id, type: "body", autofit: "shrink", anchor: "t", levels, insets: { l: 91440, t: 45720, r: 91440, b: 45720 } }} rect={{ ...rect, left: 0, top: 0 }} text={content.text} shared={{ ...shared, interactive: false }} elementId={content.id} />;
    default:
      return null;
  }
}

/** The badge of an equation: its tier and, for a declared one, its numeric check. */
export function equationBadge(cite: EqCite): { label: string; tone: "success" | "error" | "warning" | "muted"; title: string } {
  const where = `${cite.qualname} in ${cite.path}`;
  if (cite.tier === "latex") return { label: "LaTeX only", tone: "warning", title: `Display-only LaTeX of ${where}: nothing checks it against the code` };
  if (cite.tier === "derived") return { label: "derived", tone: "success", title: `Derived from the code of ${where}` };
  const c = cite.check;
  if (c.status === "agree") return { label: "declared ✓", tone: "success", title: `Declared in ${where}; agrees with the code on ${c.trials} trials (tol ${c.tol ?? "?"})` };
  if (c.status === "mismatch") {
    const ce = c.counterexample;
    const at = ce ? ` at ${Object.entries(ce.inputs).map(([k, v]) => `${k}=${v}`).join(", ")}: equation ${ce.expected}, code ${ce.actual}` : "";
    return { label: "mismatch ✗", tone: "error", title: `Declared in ${where}; disagrees with the code${at}` };
  }
  return { label: "declared ?", tone: "muted", title: `Declared in ${where}; not checked${c.reason ? `: ${c.reason}` : ""}` };
}

function EquationView({ id, cite, acknowledged, shared }: { id: string; cite: EqCite; acknowledged: boolean; shared: Shared }) {
  const stale = shared.stale.has(id);
  const badge = equationBadge(cite);
  const open = () => shared.onEquation?.(cite, id);
  return (
    <div
      className={`sp-equation${stale ? " sp-stale" : ""}${shared.onEquation ? " clickable" : ""}`}
      data-testid="slide-equation"
      data-stale={stale || undefined}
      style={{ fontSize: ptToPx(24, shared.scale) }}
      title={`${badge.title}${stale ? " (stale: the source has changed since it was placed)" : ""}`}
      onClick={shared.onEquation ? open : undefined}
    >
      <Suspense fallback={<code>{cite.latex}</code>}>
        <Katex latex={cite.latex} />
      </Suspense>
      <div className="sp-image-cites" style={{ fontSize: Math.max(8, ptToPx(10, shared.scale)) }}>
        <button
          type="button"
          className={`sp-eq-badge ${badge.tone}${stale ? " stale" : ""}`}
          data-testid="equation-badge"
          data-tier={cite.tier}
          data-check={cite.check.status}
          disabled={!shared.onEquation}
          onClick={(e) => {
            e.stopPropagation();
            open();
          }}
        >
          {badge.label}
          {acknowledged && badge.tone !== "success" ? " (ack)" : ""}
          {stale ? " ⚠" : ""}
        </button>
      </div>
    </div>
  );
}

function TableView({ table, rect, shared }: { table: TableEl; rect: Rect; shared: Shared }) {
  const { theme, scale } = shared;
  const rows = table.rows.length || 1;
  // 18 pt when the rows have room, smaller when the table is tall for its box.
  const fontPx = Math.max(ptToPx(8, scale), Math.min(ptToPx(18, scale), rect.height / rows / 2.1));
  const head = resolveColor(theme, "accent1");
  const band = mix(head, "#ffffff", 0.88);
  const line = mix(head, "#ffffff", 0.6);
  const ctx: TextStyleContext = {
    theme,
    scale,
    defaultAlign: "left",
    citeNumbers: shared.citeNumbers,
    markers: true,
    levels: [{ level: 0, font: theme.fonts.minor, size_pt: fontPx / (12700 * scale || 1), bold: false, color: "dk1", bullet: false, space_before_pt: 0, line_spacing: 1, mar_l: 0 }],
  };
  const onClick = (e: MouseEvent) => {
    const cite = (e.target as Element).closest?.(".sp-cite")?.getAttribute("data-marker");
    if (cite) shared.onCite?.(cite);
  };
  return (
    <table className="sp-table" style={{ width: rect.width, fontSize: fontPx, fontFamily: fontStack(theme.fonts.minor, theme) }} onClick={onClick}>
      <tbody>
        {table.rows.map((row, r) => {
          const header = table.header_row && r === 0;
          const banded = table.banded_rows && !header && (r - (table.header_row ? 1 : 0)) % 2 === 0;
          const bg = header ? head : banded ? band : "transparent";
          return (
            <tr key={r} className={header ? "sp-th" : undefined} style={{ background: bg, color: header ? contrastOn(head) : undefined, fontWeight: header ? 700 : undefined }}>
              {row.cells.map((cell, c) => {
                const html = textToHtml(cell.text, header ? { ...ctx, levels: [{ ...ctx.levels[0]!, bold: true, color: contrastOn(head).slice(1) }] } : ctx);
                return <td key={c} colSpan={cell.span && cell.span > 1 ? cell.span : undefined} style={{ borderBottom: `1px solid ${line}`, padding: `${fontPx * 0.35}px ${fontPx * 0.6}px` }} dangerouslySetInnerHTML={{ __html: html }} />;
              })}
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function ImageView({ content, rect, shared }: { content: Extract<PlaceholderContent, { type: "image" }>; rect: Rect; shared: Shared }) {
  const url = shared.resolveAsset?.(content.asset) ?? (/^(data|blob|https?):/.test(content.asset) ? content.asset : null);
  const cites = content.cite ?? [];
  const runCite = content.run_cite;
  const stale = shared.stale.has(content.id);
  const markers =
    cites.length || runCite ? (
      <div className="sp-image-cites" style={{ fontSize: Math.max(8, ptToPx(10, shared.scale)) }}>
        {runCite ? (
          <button
            type="button"
            className={`sp-run-cite${stale ? " stale" : ""}`}
            data-testid="run-cite-badge"
            data-run={runCite.run_id}
            title={`From run ${runCite.run_id}: ${runCite.script} ${runCite.args.join(" ")}${stale ? " (stale: the code has changed since this run)" : ""}`}
            disabled={!shared.onRunCite}
            onClick={(e) => {
              e.stopPropagation();
              shared.onRunCite?.(runCite, content.id);
            }}
          >
            {stale ? "run ⚠" : "run"}
          </button>
        ) : null}
        {cites.map((cite) => (
          <sup key={cite} className="sp-cite" data-marker={cite} title={cite} onClick={() => shared.onCite?.(cite)}>
            {shared.citeNumbers.get(cite) ?? "?"}
          </sup>
        ))}
      </div>
    ) : null;
  if (!url) {
    return (
      <div className="sp-image-missing" style={{ fontSize: ptToPx(12, shared.scale) }}>
        {content.alt || content.asset}
        {markers}
      </div>
    );
  }
  const fit = content.fit ?? "fit";
  const crop = content.crop;
  let img: ReactNode;
  if (fit === "crop" && crop) {
    // The cropped region fills the box: the image is scaled so the kept part spans it.
    const kw = Math.max(0.01, 1 - crop.l - crop.r);
    const kh = Math.max(0.01, 1 - crop.t - crop.b);
    const w = rect.width / kw;
    const h = rect.height / kh;
    img = <img src={url} alt={content.alt} draggable={false} style={{ position: "absolute", width: w, height: h, left: -crop.l * w, top: -crop.t * h, maxWidth: "none" }} />;
  } else {
    img = <img src={url} alt={content.alt} draggable={false} style={{ width: "100%", height: "100%", objectFit: fit === "fit" ? "contain" : "cover" }} />;
  }
  return (
    <div className={`sp-image${stale ? " sp-stale" : ""}`} data-fit={fit} data-stale={stale || undefined}>
      {img}
      {markers}
    </div>
  );
}

function FreeElementView({ element, shared }: { element: FreeElement; shared: Shared }) {
  const { theme, scale } = shared;
  const rect = emuRect(element, scale);
  const c = element.content;
  const levels = FREE_LEVELS(theme);
  if (c.type !== "shape") {
    return (
      <div className="sp-ph sp-object sp-free" data-element={element.id} data-type={c.type} style={{ ...rect }}>
        <ContentView content={c} rect={rect} shared={shared} levels={levels} />
      </div>
    );
  }
  const stroke = c.line ? resolveColor(theme, c.line) : c.shape === "line" || c.shape === "arrow" ? resolveColor(theme, "dk1") : "transparent";
  const sw = Math.max(1, ptToPx(1.25, scale));
  if (c.shape === "line" || c.shape === "arrow") {
    const w = Math.max(rect.width, sw);
    const h = Math.max(rect.height, sw);
    const head = Math.max(5, ptToPx(7, scale));
    const a = Math.atan2(rect.height, rect.width);
    const [x2, y2] = [rect.width, rect.height];
    return (
      <svg className="sp-free sp-shape" data-element={element.id} data-type="shape" style={{ position: "absolute", left: rect.left, top: rect.top, overflow: "visible" }} width={w} height={h}>
        <line x1={0} y1={0} x2={x2} y2={y2} stroke={stroke} strokeWidth={sw} />
        {c.shape === "arrow" ? <polygon points={`${x2},${y2} ${x2 - head * Math.cos(a - 0.4)},${y2 - head * Math.sin(a - 0.4)} ${x2 - head * Math.cos(a + 0.4)},${y2 - head * Math.sin(a + 0.4)}`} fill={stroke} /> : null}
      </svg>
    );
  }
  const fill = c.fill ? resolveColor(theme, c.fill) : "transparent";
  const radius = c.shape === "ellipse" ? "50%" : c.shape === "rounded-rect" ? Math.min(rect.width, rect.height) * 0.16 : 0;
  const onFill = c.fill ? contrastOn(fill).slice(1) : "dk1";
  return (
    <div className="sp-free sp-shape" data-element={element.id} data-type="shape" style={{ ...rect, position: "absolute", background: fill, border: c.line ? `${sw}px solid ${stroke}` : undefined, borderRadius: radius, boxSizing: "border-box" }}>
      {c.text ? (
        <TextBox
          ph={{ key: element.id, type: "ctrTitle", autofit: "shrink", anchor: "ctr", levels: levels.map((l) => ({ ...l, color: onFill, bullet: false, mar_l: 0 })), insets: { l: 91440, t: 45720, r: 91440, b: 45720 } }}
          rect={{ left: 0, top: 0, width: rect.width, height: rect.height }}
          text={c.text}
          shared={{ ...shared, interactive: false }}
        />
      ) : null}
    </div>
  );
}
