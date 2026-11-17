// Theme colours and fonts of the slide itself. The slide is a document: it always renders with the
// template's colours, whatever the app theme.
import type { LevelStyle, PlaceholderInfo, Theme } from "../../ipc";

const THEME_SLOTS = ["dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"] as const;
const FALLBACK: Record<string, string> = { dk1: "000000", lt1: "FFFFFF", dk2: "44546A", lt2: "E7E6E6", accent1: "4472C4", accent2: "ED7D31", accent3: "A5A5A5", accent4: "FFC000", accent5: "5B9BD5", accent6: "70AD47", hlink: "0563C1", folHlink: "954F72" };

/** Resolves a theme slot or a hex colour (`1F3864`, `#1F3864`) to a CSS colour. */
export function resolveColor(theme: Theme, color: string | undefined | null, fallback = "dk1"): string {
  const c = color ?? fallback;
  if ((THEME_SLOTS as readonly string[]).includes(c)) {
    const hex = (theme.colors as Record<string, string | undefined>)[c] ?? FALLBACK[c]!;
    return `#${hex.replace(/^#/, "")}`;
  }
  if (/^#?[0-9a-fA-F]{6}$/.test(c)) return `#${c.replace(/^#/, "")}`;
  return resolveColor(theme, fallback === c ? "dk1" : fallback);
}

export const ACCENTS = ["accent1", "accent2", "accent3", "accent4", "accent5", "accent6"] as const;

/** The colour of the n-th series: accent1..6 in order, then the same again lightened. */
export function seriesColor(theme: Theme, index: number, override?: string): string {
  if (override) return resolveColor(theme, override);
  const base = resolveColor(theme, ACCENTS[index % ACCENTS.length]);
  const round = Math.floor(index / ACCENTS.length);
  return round === 0 ? base : mix(base, "#ffffff", Math.min(0.6, 0.3 * round));
}

function parseHex(c: string): [number, number, number] {
  const h = c.replace(/^#/, "");
  return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
}

/** Mixes two hex colours; `t` is the share of `b`. */
export function mix(a: string, b: string, t: number): string {
  const x = parseHex(a);
  const y = parseHex(b);
  return `#${x.map((v, i) => Math.round(v + (y[i]! - v) * t).toString(16).padStart(2, "0")).join("")}`;
}

/** Black or white, whichever reads better on the fill. */
export function contrastOn(fill: string): string {
  const [r, g, b] = parseHex(fill);
  return (r * 299 + g * 587 + b * 114) / 1000 > 150 ? "#1b1f24" : "#ffffff";
}

// Metric-compatible and lookalike fallbacks for fonts a machine may lack.
const FONT_FALLBACKS: Record<string, string[]> = {
  carlito: ["Calibri", "Segoe UI", "Helvetica Neue", "Arial", "sans-serif"],
  calibri: ["Carlito", "Segoe UI", "Helvetica Neue", "Arial", "sans-serif"],
  "dejavu serif": ["Georgia", "Cambria", "Times New Roman", "serif"],
  "dejavu sans": ["Verdana", "Segoe UI", "Arial", "sans-serif"],
  "liberation sans": ["Arial", "Helvetica", "sans-serif"],
  arial: ["Liberation Sans", "Helvetica", "sans-serif"],
  "liberation serif": ["Times New Roman", "Times", "serif"],
  "times new roman": ["Liberation Serif", "Times", "serif"],
  cambria: ["Caladea", "Georgia", "serif"],
  caladea: ["Cambria", "Georgia", "serif"],
  "noto sans": ["Segoe UI", "Helvetica Neue", "Arial", "sans-serif"],
  "noto serif": ["Georgia", "Times New Roman", "serif"],
};
const SERIF_HINT = /serif|times|georgia|garamond|cambria|caladea|minion|palatino|book/i;

/** A CSS font-family list for a template font name (`+mj-lt` / `+mn-lt` resolve to the theme fonts). */
export function fontStack(font: string, theme: Theme): string {
  const name = font === "+mj-lt" ? theme.fonts.major : font === "+mn-lt" ? theme.fonts.minor : font || theme.fonts.minor;
  const rest = FONT_FALLBACKS[name.toLowerCase()] ?? (SERIF_HINT.test(name) && !/sans/i.test(name) ? ["Georgia", "serif"] : ["Segoe UI", "Helvetica Neue", "Arial", "sans-serif"]);
  return [name, ...rest].map((f) => (/^[a-z-]+$/.test(f) ? f : `"${f.replace(/"/g, "")}"`)).join(", ");
}

const DEFAULT_LEVEL: LevelStyle = { level: 0, font: "", size_pt: 18, bold: false, color: "dk1", bullet: false, space_before_pt: 0, line_spacing: 1, mar_l: 0 };

/** The style of an indent level: the exact level, else the deepest one the placeholder defines. */
export function levelStyle(levels: LevelStyle[], level: number): LevelStyle {
  if (levels.length === 0) return { ...DEFAULT_LEVEL, level };
  return levels.find((l) => l.level === level) ?? levels.reduce((a, b) => (b.level <= level && b.level > a.level ? b : a), levels[0]!);
}

/**
 * Horizontal alignment of a placeholder's text. The template description carries no alignment, so
 * this follows the conventions of the default masters: centred titles, subtitles and footers,
 * right-aligned slide numbers, left-aligned bodies.
 */
export function defaultAlign(p: Pick<PlaceholderInfo, "type">): "left" | "center" | "right" {
  if (p.type === "title" || p.type === "ctrTitle" || p.type === "subTitle" || p.type === "ftr") return "center";
  if (p.type === "sldNum") return "right";
  return "left";
}
