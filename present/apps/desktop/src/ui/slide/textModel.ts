// The text model on the DOM: `Text` (paragraphs of runs, PROTOCOL 2.2) to HTML and back, and the
// editing operations the canvas allows (new paragraph, level change). The DOM shape is
//   <p data-level data-bullet? data-align?><span data-b? data-i? data-u? data-color? data-link? data-cite?>text</span>…</p>
// and the parser also accepts what the browser's own editing commands produce (<b>, <i>, <u>,
// font-weight / font-style spans, <div>, <br>), so a contentEditable round trip stays in the model.
import type { LevelStyle, Paragraph, Run, Text, Theme } from "../../ipc";
import { fontStack, levelStyle, resolveColor } from "./theme";

export interface TextStyleContext {
  theme: Theme;
  levels: LevelStyle[];
  /** Pixels per EMU. */
  scale: number;
  defaultAlign: "left" | "center" | "right";
  /** Numbers of the slide's citations (1-based); markers are drawn only when given. */
  citeNumbers?: Map<string, number>;
  /** Draw citation markers (display mode); never while editing. */
  markers?: boolean;
}

export const MAX_LEVEL = 4;

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** Whether a paragraph draws a bullet, and which kind. */
export function bulletKind(p: Pick<Paragraph, "bullet">, style: LevelStyle): "none" | "bullet" | "number" {
  if (p.bullet === "none") return "none";
  if (p.bullet === "number") return "number";
  return style.bullet ? "bullet" : "none";
}

/** Inline style of a paragraph element for its level. Sizes scale with `--fit` (the autofit factor). */
export function paragraphStyle(p: Pick<Paragraph, "level" | "bullet" | "align">, ctx: TextStyleContext, first: boolean): string {
  const level = Math.min(MAX_LEVEL, Math.max(0, p.level ?? 0));
  const st = levelStyle(ctx.levels, level);
  const px = st.size_pt * 12700 * ctx.scale;
  const kind = bulletKind(p, st);
  const marL = st.mar_l * ctx.scale;
  const align = p.align && p.align !== "auto" ? p.align : ctx.defaultAlign;
  const parts = [
    `font-family:${fontStack(st.font, ctx.theme).replace(/"/g, "'")}`,
    `font-size:calc(${px.toFixed(3)}px * var(--fit, 1))`,
    `font-weight:${st.bold ? 700 : 400}`,
    `color:${resolveColor(ctx.theme, st.color)}`,
    `line-height:${(1.2 * (st.line_spacing || 1)).toFixed(3)}`,
    `margin:${first ? 0 : `calc(${(st.space_before_pt * 12700 * ctx.scale).toFixed(3)}px * var(--fit, 1))`} 0 0 0`,
    `text-align:${align}`,
  ];
  // The bullet hangs in the left margin of the level; without one, the margin is plain indent.
  if (kind !== "none") parts.push(`padding-left:max(${marL.toFixed(3)}px, 1.1em)`);
  else if (marL > 0) parts.push(`padding-left:${marL.toFixed(3)}px`);
  return parts.join(";");
}

function runStyle(r: Run, theme: Theme): string {
  const parts: string[] = [];
  if (r.b) parts.push("font-weight:700");
  if (r.i) parts.push("font-style:italic");
  if (r.u || r.link) parts.push("text-decoration:underline");
  if (r.color) parts.push(`color:${resolveColor(theme, r.color)}`);
  else if (r.link) parts.push(`color:${resolveColor(theme, "hlink")}`);
  return parts.join(";");
}

function runHtml(r: Run, ctx: TextStyleContext, markers: string[]): string {
  const attrs: string[] = [];
  if (r.b) attrs.push('data-b="1"');
  if (r.i) attrs.push('data-i="1"');
  if (r.u) attrs.push('data-u="1"');
  if (r.color) attrs.push(`data-color="${esc(r.color)}"`);
  if (r.link) attrs.push(`data-link="${esc(r.link)}"`);
  if (r.cite !== undefined) attrs.push(`data-cite="${esc(JSON.stringify(r.cite))}"`);
  const style = runStyle(r, ctx.theme);
  if (style) attrs.push(`style="${style}"`);
  let html = `<span ${attrs.join(" ")}>${esc(r.t)}</span>`;
  for (const cite of markers) {
    const n = ctx.citeNumbers?.get(cite);
    if (n !== undefined) html += `<sup class="sp-cite" contenteditable="false" data-marker="${esc(cite)}" title="${esc(cite)}">${n}</sup>`;
  }
  return html;
}

function paragraphHtml(p: Paragraph, ctx: TextStyleContext, first: boolean): string {
  const level = Math.min(MAX_LEVEL, Math.max(0, p.level ?? 0));
  const kind = bulletKind(p, levelStyle(ctx.levels, level));
  const attrs = [`data-level="${level}"`, `data-kind="${kind}"`];
  if (p.bullet) attrs.push(`data-bullet="${p.bullet}"`);
  if (p.align) attrs.push(`data-align="${p.align}"`);
  // A citation shared by consecutive runs gets one marker, after the last of them.
  const runs = p.runs.map((r, i) => {
    const next = p.runs[i + 1]?.cite ?? [];
    return runHtml(r, ctx, ctx.markers ? (r.cite ?? []).filter((c) => !next.includes(c)) : []);
  });
  const empty = p.runs.every((r) => r.t === "");
  return `<p ${attrs.join(" ")} style="${paragraphStyle(p, ctx, first)}">${empty ? "<br>" : runs.join("")}</p>`;
}

/** HTML of a text body. With `ctx.markers`, cited runs are followed by superscript citation numbers. */
export function textToHtml(text: Text, ctx: TextStyleContext): string {
  return text.paragraphs.map((p, i) => paragraphHtml(p, ctx, i === 0)).join("");
}

// ---- DOM -> model ----

interface Fmt {
  b?: boolean;
  i?: boolean;
  u?: boolean;
  color?: string;
  link?: string;
  cite?: string[];
}

function fmtOf(node: Node, stop: Element): Fmt {
  const f: Fmt = {};
  const seen = { b: false, i: false, u: false };
  for (let el = node.parentElement; el && el !== stop; el = el.parentElement) {
    const tag = el.tagName;
    const st = (el as HTMLElement).style;
    if (!seen.b) {
      const w = st?.fontWeight;
      if (w) {
        seen.b = true;
        f.b = w === "bold" || w === "bolder" || Number(w) >= 600;
      } else if (tag === "B" || tag === "STRONG" || el.hasAttribute("data-b")) {
        seen.b = true;
        f.b = true;
      }
    }
    if (!seen.i) {
      const s = st?.fontStyle;
      if (s) {
        seen.i = true;
        f.i = s === "italic" || s === "oblique";
      } else if (tag === "I" || tag === "EM" || el.hasAttribute("data-i")) {
        seen.i = true;
        f.i = true;
      }
    }
    if (!seen.u && (tag === "U" || el.hasAttribute("data-u"))) {
      seen.u = true;
      f.u = true;
    }
    if (f.color === undefined && el.hasAttribute("data-color")) f.color = el.getAttribute("data-color") ?? undefined;
    if (f.link === undefined) {
      const link = el.getAttribute("data-link") ?? (tag === "A" ? el.getAttribute("href") : null);
      if (link) f.link = link;
    }
    if (f.cite === undefined && el.hasAttribute("data-cite")) {
      try {
        const v: unknown = JSON.parse(el.getAttribute("data-cite") ?? "");
        if (Array.isArray(v)) f.cite = v.filter((x): x is string => typeof x === "string");
      } catch {
        // not a citation list
      }
    }
  }
  return f;
}

function makeRun(t: string, f: Fmt): Run {
  const r: Run = { t };
  if (f.b) r.b = true;
  if (f.i) r.i = true;
  if (f.u) r.u = true;
  if (f.color) r.color = f.color as Run["color"];
  if (f.link) r.link = f.link;
  if (f.cite !== undefined) r.cite = f.cite;
  return r;
}

function sameFormat(a: Run, b: Run): boolean {
  return !!a.b === !!b.b && !!a.i === !!b.i && !!a.u === !!b.u && a.color === b.color && a.link === b.link && JSON.stringify(a.cite) === JSON.stringify(b.cite);
}

/** Joins neighbouring runs with the same formatting and drops empty ones (keeping one for an empty paragraph). */
export function normalizeRuns(runs: Run[]): Run[] {
  const out: Run[] = [];
  for (const r of runs) {
    if (r.t === "") continue;
    const last = out[out.length - 1];
    if (last && sameFormat(last, r)) last.t += r.t;
    else out.push({ ...r });
  }
  return out.length ? out : [{ t: "" }];
}

function paragraphOf(el: Element): Paragraph {
  const runs: Run[] = [];
  const walk = (node: Node) => {
    for (const child of Array.from(node.childNodes)) {
      if (child.nodeType === 3) {
        const t = (child.nodeValue ?? "").replace(/\u00a0/g, " ").replace(/[\r\n]+/g, " ");
        if (t) runs.push(makeRun(t, fmtOf(child, el)));
      } else if (child.nodeType === 1) {
        const c = child as Element;
        if (c.classList.contains("sp-cite") || c.tagName === "BR") continue;
        walk(c);
      }
    }
  };
  walk(el);
  const p: Paragraph = { runs: normalizeRuns(runs) };
  const level = Number(el.getAttribute("data-level") ?? "0");
  if (Number.isFinite(level) && level > 0) p.level = Math.min(MAX_LEVEL, Math.round(level));
  const bullet = el.getAttribute("data-bullet");
  if (bullet === "auto" || bullet === "none" || bullet === "number") p.bullet = bullet;
  const align = el.getAttribute("data-align");
  if (align === "auto" || align === "left" || align === "center" || align === "right") p.align = align;
  return p;
}

const BLOCKS = new Set(["P", "DIV", "LI", "H1", "H2", "H3"]);

/** Reads a text body back from the editing DOM. Loose inline content between blocks forms its own paragraph. */
export function domToText(root: Element): Text {
  const paragraphs: Paragraph[] = [];
  let loose: Element | null = null;
  const flush = () => {
    if (loose) {
      paragraphs.push(paragraphOf(loose));
      loose = null;
    }
  };
  for (const child of Array.from(root.childNodes)) {
    if (child.nodeType === 1 && BLOCKS.has((child as Element).tagName)) {
      flush();
      paragraphs.push(paragraphOf(child as Element));
    } else if (child.nodeType === 1 && (child as Element).tagName === "BR") {
      flush();
    } else if (child.nodeType === 3 && !(child.nodeValue ?? "").trim()) {
      continue;
    } else {
      loose ??= root.ownerDocument.createElement("p");
      loose.appendChild(child.cloneNode(true));
    }
  }
  flush();
  return { paragraphs: paragraphs.length ? paragraphs : [{ runs: [{ t: "" }] }] };
}

/**
 * Canonical form used to compare texts: level 0 and absent level are the same, as are absent and
 * false emphasis flags.
 */
export function canonicalText(text: Text): string {
  return JSON.stringify(
    text.paragraphs.map((p) => ({
      l: p.level ?? 0,
      b: p.bullet ?? null,
      a: p.align ?? null,
      r: normalizeRuns(p.runs).map((r) => [r.t, !!r.b, !!r.i, !!r.u, r.color ?? null, r.link ?? null, r.cite ?? null]),
    })),
  );
}

/**
 * The edited text, keeping the original's optional fields where nothing changed: an untouched
 * paragraph is returned as it was, so a no-op edit produces an identical document.
 */
export function mergeEdited(original: Text, edited: Text): Text {
  const paragraphs = edited.paragraphs.map((p, i) => {
    const o = original.paragraphs[i];
    if (!o) return p;
    if (canonicalText({ paragraphs: [o] }) === canonicalText({ paragraphs: [p] })) return o;
    // A changed paragraph keeps the original's explicit `level: 0`, which the DOM cannot tell from an absent level.
    return o.level === 0 && p.level === undefined ? { level: 0, ...p } : p;
  });
  return { ...original, paragraphs };
}

// ---- editing operations on the DOM ----

/** The paragraph element containing a node, inside the editing root. */
export function paragraphElement(node: Node | null, root: Element): HTMLElement | null {
  for (let n: Node | null = node; n && n !== root; n = n.parentNode) {
    if (n.nodeType === 1 && n.parentNode === root) return n as HTMLElement;
  }
  return null;
}

/**
 * Enter: splits the paragraph at a collapsed range into two paragraphs of the same level and
 * returns the new (second) one. Formatting spans are split with their attributes.
 */
export function splitParagraph(root: Element, range: Range): HTMLElement | null {
  const p = paragraphElement(range.startContainer, root);
  if (!p) return null;
  const tail = range.cloneRange();
  tail.setEndAfter(p.lastChild ?? p);
  const fragment = tail.extractContents();
  const next = p.cloneNode(false) as HTMLElement;
  next.appendChild(fragment);
  for (const el of [p, next]) {
    // Drop empty shells the split leaves behind; an empty paragraph keeps a <br> so it has a line box.
    for (const span of Array.from(el.querySelectorAll("span, b, i, u, strong, em"))) if (!span.textContent && !span.querySelector("br")) span.remove();
    if (!el.textContent) el.innerHTML = "<br>";
  }
  p.after(next);
  return next;
}

/** Tab / Shift+Tab: the new level of a paragraph element, clamped to 0..MAX_LEVEL. */
export function shiftLevel(p: HTMLElement, delta: 1 | -1, ctx?: TextStyleContext): number {
  const cur = Number(p.getAttribute("data-level") ?? "0") || 0;
  const level = Math.min(MAX_LEVEL, Math.max(0, cur + delta));
  p.setAttribute("data-level", String(level));
  if (ctx) restyleParagraph(p, ctx);
  return level;
}

/** Re-applies the level style to a paragraph element after its level changed. */
export function restyleParagraph(p: HTMLElement, ctx: TextStyleContext): void {
  const level = Number(p.getAttribute("data-level") ?? "0") || 0;
  const bullet = p.getAttribute("data-bullet") as Paragraph["bullet"] | null;
  const align = p.getAttribute("data-align") as Paragraph["align"] | null;
  const model = { level, ...(bullet ? { bullet } : {}), ...(align ? { align } : {}) };
  p.setAttribute("style", paragraphStyle(model, ctx, p.previousElementSibling === null));
  p.setAttribute("data-kind", bulletKind(model, levelStyle(ctx.levels, level)));
}
