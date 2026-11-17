// EMU geometry of the live preview: everything is the template's EMU box scaled by one factor.
import type { Deck, LayoutInfo, PlaceholderInfo, Size, TemplateDescription } from "../../ipc";

export const EMU_PER_PT = 12700;
export const EMU_PER_INCH = 914400;

export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export function slideSize(template: TemplateDescription, deck?: Deck | null): Size {
  return deck?.slide_size ?? template.slide_size;
}

/** Pixels per EMU for a slide drawn `width` pixels wide. */
export function scaleFor(size: Size, width: number): number {
  return size.cx > 0 ? width / size.cx : 0;
}

export function slideHeight(size: Size, width: number): number {
  return size.cx > 0 ? (width * size.cy) / size.cx : 0;
}

export function emuRect(box: { x: number; y: number; cx: number; cy: number }, scale: number): Rect {
  return { left: box.x * scale, top: box.y * scale, width: box.cx * scale, height: box.cy * scale };
}

/** A point size in pixels at the given scale. */
export function ptToPx(pt: number, scale: number): number {
  return pt * EMU_PER_PT * scale;
}

/** The pixel boxes of a layout's placeholders, keyed by placeholder key. */
export function layoutRects(layout: LayoutInfo, scale: number): Record<string, Rect> {
  return Object.fromEntries(layout.placeholders.map((p) => [p.key, emuRect(p, scale)]));
}

/** The text area of a placeholder: its box minus the insets. */
export function textRect(p: PlaceholderInfo, scale: number): Rect {
  const r = emuRect(p, scale);
  return {
    left: r.left + p.insets.l * scale,
    top: r.top + p.insets.t * scale,
    width: Math.max(0, r.width - (p.insets.l + p.insets.r) * scale),
    height: Math.max(0, r.height - (p.insets.t + p.insets.b) * scale),
  };
}
