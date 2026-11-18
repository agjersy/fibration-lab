// Data and scale helpers of the SVG charts (no charting library).
import type { CellValue, Chart, Table } from "../../../deck/types";

export interface ChartSeriesData {
  name: string;
  values: (number | null)[];
  type: "bar" | "line";
  axis: "primary" | "secondary";
  color?: string;
}

export interface ChartData {
  categories: string[];
  series: ChartSeriesData[];
}

function num(v: CellValue | undefined): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v === "string" && v.trim() !== "" && Number.isFinite(Number(v))) return Number(v);
  return null;
}

/** The values a chart draws: `computed` when the core wrote it, else the inline table. */
export function chartTable(chart: Chart): Table | null {
  if (chart.computed) return chart.computed;
  return chart.source.kind === "inline" ? chart.source.table : null;
}

function transpose(table: Table): Table {
  const columns = [table.columns[0] ?? "", ...table.rows.map((r) => String(r[0] ?? ""))];
  const rows = table.columns.slice(1).map((name, i) => [name, ...table.rows.map((r) => r[i + 1] ?? null)] as CellValue[]);
  return { columns, rows, loc: table.loc };
}

export function chartData(chart: Chart): ChartData | null {
  const raw = chartTable(chart);
  if (!raw || raw.columns.length === 0) return null;
  const table = chart.series_from === "rows" ? transpose(raw) : raw;
  const catIndex = Math.max(0, chart.category ? table.columns.indexOf(chart.category) : 0);
  const categories = table.rows.map((r) => String(r[catIndex] ?? ""));
  const column = (name: string) => table.columns.indexOf(name);
  const defaultType = chart.type === "line" || chart.type === "area" || chart.type === "scatter" ? "line" : "bar";
  let series: ChartSeriesData[];
  if (chart.series && chart.series.length > 0 && chart.series_from !== "rows") {
    series = chart.series.flatMap((s) => {
      const i = column(s.column);
      if (i < 0) return [];
      return [{ name: s.name, values: table.rows.map((r) => num(r[i])), type: chart.type === "combo" ? (s.type ?? "bar") : defaultType, axis: s.axis ?? "primary", ...(s.color ? { color: s.color } : {}) }];
    });
  } else {
    series = table.columns.flatMap((name, i) => {
      if (i === catIndex) return [];
      const values = table.rows.map((r) => num(r[i]));
      if (values.every((v) => v === null)) return [];
      return [{ name, values, type: defaultType, axis: "primary" as const }];
    });
  }
  return { categories, series };
}

export interface Scale {
  min: number;
  max: number;
  step: number;
  ticks: number[];
}

/** A "nice" linear scale covering [lo, hi] with about `count` ticks; bar charts pass `zero` to include 0. */
export function niceScale(lo: number, hi: number, count = 5, zero = true): Scale {
  if (!Number.isFinite(lo) || !Number.isFinite(hi)) return { min: 0, max: 1, step: 0.2, ticks: [0, 0.2, 0.4, 0.6, 0.8, 1] };
  if (zero) {
    lo = Math.min(0, lo);
    hi = Math.max(0, hi);
  }
  if (lo === hi) {
    if (lo === 0) hi = 1;
    else if (lo > 0) lo = zero ? 0 : lo * 0.9;
    else hi = zero ? 0 : hi * 0.9;
  }
  const raw = (hi - lo) / Math.max(1, count);
  const mag = 10 ** Math.floor(Math.log10(raw));
  const norm = raw / mag;
  const step = (norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 2.5 ? 2.5 : norm <= 5 ? 5 : 10) * mag;
  const clean = (v: number) => Number(v.toPrecision(12));
  const min = clean(Math.floor(lo / step + 1e-9) * step);
  const max = clean(Math.ceil(hi / step - 1e-9) * step);
  const ticks: number[] = [];
  for (let i = 0; i <= Math.round((max - min) / step); i++) ticks.push(Number((min + i * step).toPrecision(12)));
  return { min, max, step: clean(step), ticks };
}

/** Maps a value to a pixel position on an axis from `from` (at scale.min) to `to` (at scale.max). */
export function project(scale: Scale, value: number, from: number, to: number): number {
  return scale.max === scale.min ? from : from + ((value - scale.min) / (scale.max - scale.min)) * (to - from);
}

/** Per-category stacks: for each series the [start, end] of its segment. `percent` normalises to 100. */
export function stack(series: ChartSeriesData[], count: number, percent: boolean): [number, number][][] {
  const pos = new Array<number>(count).fill(0);
  const neg = new Array<number>(count).fill(0);
  const totals = Array.from({ length: count }, (_, c) => series.reduce((n, s) => n + Math.abs(s.values[c] ?? 0), 0));
  return series.map((s) =>
    Array.from({ length: count }, (_, c): [number, number] => {
      let v = s.values[c] ?? 0;
      if (percent) v = totals[c] ? (v / totals[c]!) * 100 : 0;
      if (v >= 0) {
        const start = pos[c]!;
        pos[c] = start + v;
        return [start, start + v];
      }
      const start = neg[c]!;
      neg[c] = start + v;
      return [start, start + v];
    }),
  );
}

/**
 * Formats a number with the common Excel format codes: decimals from the code (`0.0`), thousands
 * separator (`#,##0`), percent (`0.0%`). Without a code: up to two decimals, grouped.
 */
export function formatNumber(value: number, format?: string | null): string {
  if (!Number.isFinite(value)) return "";
  if (!format || format === "General") {
    const abs = Math.abs(value);
    const digits = abs >= 100 || Number.isInteger(value) ? 0 : abs >= 10 ? 1 : 2;
    return trimZeros(group(value.toFixed(digits)));
  }
  const percent = format.includes("%");
  const m = /\.(0+)/.exec(format);
  const digits = m ? m[1]!.length : 0;
  const s = (percent ? value * 100 : value).toFixed(digits);
  return `${format.includes(",") ? group(s) : s}${percent ? "%" : ""}`;
}

function group(s: string): string {
  const [int, frac] = s.split(".");
  const g = int!.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return frac !== undefined ? `${g}.${frac}` : g;
}

function trimZeros(s: string): string {
  return s.includes(".") ? s.replace(/\.?0+$/, "") : s;
}

/** Angles (radians, clockwise from 12 o'clock) of pie slices for the non-negative values. */
export function pieAngles(values: (number | null)[]): [number, number][] {
  const vs = values.map((v) => Math.max(0, v ?? 0));
  const total = vs.reduce((a, b) => a + b, 0);
  let at = 0;
  return vs.map((v) => {
    const start = at;
    at += total > 0 ? (v / total) * Math.PI * 2 : 0;
    return [start, at];
  });
}
