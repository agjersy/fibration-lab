// Pure helpers over the deck document (PROTOCOL 2) and the template description (PROTOCOL 3), shared
// by the stores, the views, the preview renderer and the mock backend.
import type { Deck, LayoutInfo, Paragraph, PlaceholderContent, PlaceholderInfo, Run, Slide, TemplateDescription, Text } from "./types";

export function runsText(runs: Run[]): string {
  return runs.map((r) => r.t).join("");
}

export function paragraphText(p: Paragraph): string {
  return runsText(p.runs);
}

export function plainText(text: Text | undefined | null): string {
  return text ? text.paragraphs.map(paragraphText).join("\n") : "";
}

export function textOf(s: string): Text {
  return { paragraphs: s.split("\n").map((line) => ({ runs: [{ t: line }] })) };
}

/** Key of the slide's title placeholder (`title`, else the first key starting with it). */
export function titleKey(slide: Slide): string | null {
  if (slide.placeholders.title) return "title";
  return Object.keys(slide.placeholders).find((k) => k.startsWith("title")) ?? null;
}

export function slideTitle(slide: Slide): string {
  const key = titleKey(slide);
  const c = key ? slide.placeholders[key] : undefined;
  return c && c.type === "text" ? plainText(c.text).replace(/\n/g, " ") : "";
}

/** A copy of the slide with its title replaced; other run formatting of the title is dropped. */
export function withTitle(slide: Slide, title: string, template?: TemplateDescription): Slide {
  const key = titleKey(slide) ?? (template ? (findLayout(template, slide.layout)?.placeholders.find((p) => isTitleType(p))?.key ?? null) : null);
  if (!key) return slide;
  const cur = slide.placeholders[key];
  const id = cur && cur.type !== "empty" ? cur.id : newElementId(slide);
  return { ...slide, placeholders: { ...slide.placeholders, [key]: { type: "text", id, text: textOf(title) } } };
}

export function isTitleType(p: PlaceholderInfo): boolean {
  return p.type === "title" || p.type === "ctrTitle";
}

/** Placeholders the document never fills: date, footer and slide number come from `deck.footer`. */
export function isFurniture(p: PlaceholderInfo): boolean {
  return p.type === "dt" || p.type === "ftr" || p.type === "sldNum";
}

export function allLayouts(template: TemplateDescription): LayoutInfo[] {
  return template.masters.flatMap((m) => m.layouts);
}

export function findLayout(template: TemplateDescription, name: string): LayoutInfo | undefined {
  return allLayouts(template).find((l) => l.name === name);
}

export function elementIds(slide: Slide): string[] {
  const ids: string[] = [];
  for (const c of Object.values(slide.placeholders)) if (c && c.type !== "empty") ids.push(c.id);
  for (const e of slide.elements) ids.push(e.id);
  return ids;
}

/** A fresh element id (`^e-[0-9A-Za-z][0-9A-Za-z-]{0,31}$`) not used on the slide. */
export function newElementId(slide: Slide, taken: string[] = []): string {
  const used = new Set([...elementIds(slide), ...taken]);
  for (let n = 1; ; n++) {
    const id = `e-u${n}`;
    if (!used.has(id)) return id;
  }
}

function kindOf(c: PlaceholderContent): "text" | "image" | "chart" | "table" | "diagram" | null {
  // An equation is a text box holding a native equation.
  return c.type === "empty" ? null : c.type === "equation" ? "text" : c.type;
}

/**
 * Switches a slide to another layout, carrying content over. Content keeps its placeholder key where
 * the new layout has that key and accepts the content kind; what is left is placed, in order, into
 * the first free placeholder that accepts it (so `subtitle` text lands in `body`); the rest is
 * dropped and reported.
 */
export function switchLayout(slide: Slide, layout: LayoutInfo): { slide: Slide; dropped: string[] } {
  const targets = layout.placeholders.filter((p) => !isFurniture(p));
  const accepts = (p: PlaceholderInfo, c: PlaceholderContent) => {
    const k = kindOf(c);
    return k !== null && p.accepts.includes(k);
  };
  const next: Record<string, PlaceholderContent> = {};
  const leftovers: [string, PlaceholderContent][] = [];
  for (const [key, content] of Object.entries(slide.placeholders)) {
    if (!content || content.type === "empty") continue;
    const target = targets.find((p) => p.key === key);
    if (target && accepts(target, content)) next[key] = content;
    else leftovers.push([key, content]);
  }
  const dropped: string[] = [];
  for (const [key, content] of leftovers) {
    const wasTitle = key.startsWith("title");
    const free = targets.find((p) => !(p.key in next) && accepts(p, content) && isTitleType(p) === wasTitle);
    if (free) next[free.key] = content;
    else dropped.push(key);
  }
  return { slide: { ...slide, layout: layout.name, placeholders: next }, dropped };
}

export function orderedSlideIds(deck: Deck): string[] {
  return deck.sections.flatMap((s) => s.slides);
}

export function sectionOf(deck: Deck, slideId: string): string {
  return deck.sections.find((s) => s.slides.includes(slideId))?.title ?? "";
}

/**
 * Moves a slide to sit after `after` (or to the start of `section`, or of the deck, when `after` is
 * absent). Returns a new deck; unknown ids leave it unchanged.
 */
export function moveSlide(deck: Deck, slideId: string, after?: string | null, section?: string | null): Deck {
  if (!orderedSlideIds(deck).includes(slideId) || after === slideId) return deck;
  const sections = deck.sections.map((s) => ({ ...s, slides: s.slides.filter((id) => id !== slideId) }));
  if (after) {
    const target = sections.find((s) => s.slides.includes(after));
    if (!target) return deck;
    target.slides.splice(target.slides.indexOf(after) + 1, 0, slideId);
  } else {
    const target = (section ? sections.find((s) => s.title === section) : undefined) ?? sections[0];
    if (!target) return deck;
    target.slides.unshift(slideId);
  }
  return { ...deck, sections };
}

/** The id a slide sits after in deck order, or null for the first. */
export function previousSlideId(deck: Deck, slideId: string): string | null {
  const ids = orderedSlideIds(deck);
  const i = ids.indexOf(slideId);
  return i > 0 ? (ids[i - 1] ?? null) : null;
}

export interface ParsedCite {
  ref_id: string;
  version: string;
  loc: string;
}

/** Parses `ref:<ref_id>@<version>#<loc>` (PROTOCOL 2.3). */
export function parseCite(cite: string): ParsedCite | null {
  const m = /^ref:(r-[0-9a-z][0-9a-z-]{0,23})@([0-9a-f]+)#(.+)$/.exec(cite);
  return m ? { ref_id: m[1]!, version: m[2]!, loc: m[3]! } : null;
}

/** Every citation on a slide, in reading order, without duplicates. */
export function slideCites(slide: Slide): string[] {
  const out: string[] = [];
  const add = (cs: string[] | undefined) => {
    for (const c of cs ?? []) if (!out.includes(c)) out.push(c);
  };
  const fromText = (t: Text | undefined) => {
    for (const para of t?.paragraphs ?? []) for (const r of para.runs) add(r.cite);
  };
  const fromContent = (c: PlaceholderContent | { type: "shape"; text?: Text }) => {
    if (c.type === "text") fromText(c.text);
    else if (c.type === "image") add(c.cite);
    else if (c.type === "chart") {
      if (c.chart.source.kind === "ref") add([c.chart.source.cite]);
      else add(c.chart.source.cites);
    } else if (c.type === "table") {
      for (const row of c.table.rows) for (const cell of row.cells) fromText(cell.text);
      if (c.table.source?.kind === "ref") add([c.table.source.cite]);
    } else if (c.type === "shape") fromText(c.text);
  };
  for (const c of Object.values(slide.placeholders)) if (c) fromContent(c);
  for (const e of slide.elements) fromContent(e.content);
  return out;
}
