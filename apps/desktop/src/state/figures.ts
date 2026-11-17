// One figure, one entry: a run may write the same figure in several formats
// (`residuals.png`, `residuals.svg`, `residuals.pdf`). Files are grouped by
// stem within a run; the PNG is what gets displayed and the other formats are
// offered as alternates (IPC.md section 8).
import { convertFileSrc } from "@tauri-apps/api/core";
import { hasTauri } from "../ipc/backend";
import type { FigureView } from "../ipc";

export interface FigureEntry {
  /** File name without directory or extension; the tab label and selection key. */
  name: string;
  /** The file to display: PNG when present, then SVG, then PDF. */
  primary: FigureView;
  /** Every format of this figure, the primary first. */
  formats: FigureView[];
}

const PREFERENCE: Record<FigureView["format"], number> = { png: 0, svg: 1, pdf: 2 };

/** File name of the figure without its extension (`out/x/figures/fit.png` gives `fit`). */
export function figureStem(figure: Pick<FigureView, "path" | "name">): string {
  const base = figure.path.slice(figure.path.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  const stem = dot > 0 ? base.slice(0, dot) : base;
  return stem || figure.name;
}

/** Groups the figure files of one run by stem, in order of first appearance. */
export function groupFigures(figures: readonly FigureView[]): FigureEntry[] {
  const byStem = new Map<string, FigureView[]>();
  for (const f of figures) {
    const stem = figureStem(f);
    const list = byStem.get(stem);
    if (list) list.push(f);
    else byStem.set(stem, [f]);
  }
  const out: FigureEntry[] = [];
  for (const [name, files] of byStem) {
    const formats = [...files].sort((a, b) => PREFERENCE[a.format] - PREFERENCE[b.format]);
    const primary = formats[0];
    if (primary) out.push({ name, primary, formats });
  }
  return out;
}

/**
 * URL the webview can load directly, when the file is on this machine and the
 * shell is Tauri (asset protocol: no IPC call, no base64 copy, browser caching).
 * Null means the bytes have to come through `figure_data`.
 */
export function figureAssetUrl(figure: Pick<FigureView, "file_abs">): string | null {
  if (figure.file_abs === null || !hasTauri()) return null;
  return convertFileSrc(figure.file_abs);
}
