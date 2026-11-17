// Charts of the live preview, drawn as SVG from `chart.computed` (or the inline table). Theme
// colours accent1..6 in order, value axis with gridlines, legend; no charting library.
import type { ReactNode } from "react";
import type { Chart, Theme } from "../../ipc";
import { chartData, formatNumber, niceScale, pieAngles, project, stack } from "./chartModel";
import type { ChartData, ChartSeriesData, Scale } from "./chartModel";
import { fontStack, mix, resolveColor, seriesColor } from "./theme";

interface ChartViewProps {
  chart: Chart;
  theme: Theme;
  width: number;
  height: number;
  /** Font size of labels in pixels. */
  fontPx: number;
}

interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

const textW = (s: string, fs: number) => s.length * fs * 0.56;

function truncate(s: string, maxPx: number, fs: number): string {
  const max = Math.max(1, Math.floor(maxPx / (fs * 0.56)));
  return s.length > max ? `${s.slice(0, Math.max(1, max - 1))}…` : s;
}

export function ChartView({ chart, theme, width, height, fontPx }: ChartViewProps) {
  const data = chartData(chart);
  const fs = fontPx;
  const ink = resolveColor(theme, "dk1");
  const muted = mix(ink, "#ffffff", 0.35);
  const grid = mix(ink, "#ffffff", 0.85);
  const font = fontStack(theme.fonts.minor, theme);
  if (!data || data.series.length === 0 || width < 8 || height < 8) {
    return (
      <div className="sp-chart-empty" style={{ fontSize: fs, color: muted, fontFamily: font }}>
        {data ? "No numeric series to draw" : "Chart values not computed yet"}
      </div>
    );
  }
  const colors = data.series.map((s, i) => seriesColor(theme, i, s.color));
  const isPie = chart.type === "pie" || chart.type === "doughnut";
  const sliceColors = data.categories.map((_, i) => seriesColor(theme, i));
  const legendItems = isPie ? data.categories.map((name, i) => ({ name, color: sliceColors[i]! })) : data.series.map((s, i) => ({ name: s.name, color: colors[i]!, line: s.type === "line" && chart.type !== "area" }));
  const legendPos = chart.legend ?? (isPie ? "right" : legendItems.length > 1 ? "bottom" : "none");

  const pad = fs * 0.6;
  let area: Box = { x: pad, y: pad, w: width - 2 * pad, h: height - 2 * pad };
  const parts: ReactNode[] = [];

  if (chart.title) {
    parts.push(
      <text key="title" x={width / 2} y={area.y + fs * 1.15} textAnchor="middle" fontSize={fs * 1.25} fontWeight={700} fill={ink}>
        {truncate(chart.title, area.w, fs * 1.25)}
      </text>,
    );
    area = { ...area, y: area.y + fs * 2, h: area.h - fs * 2 };
  }

  if (legendPos !== "none" && legendItems.length > 0) {
    const sw = fs * 0.9;
    if (legendPos === "right") {
      const lw = Math.min(area.w * 0.32, Math.max(...legendItems.map((it) => textW(it.name, fs))) + sw + fs * 1.2);
      const x0 = area.x + area.w - lw + fs * 0.4;
      const rowH = fs * 1.5;
      const y0 = area.y + Math.max(0, (area.h - rowH * legendItems.length) / 2);
      legendItems.forEach((it, i) => parts.push(<LegendEntry key={`lg${i}`} x={x0} y={y0 + i * rowH + rowH / 2} sw={sw} fs={fs} ink={ink} item={{ ...it, name: truncate(it.name, lw - sw - fs, fs) }} />));
      area = { ...area, w: area.w - lw };
    } else {
      const widths = legendItems.map((it) => sw + fs * 0.4 + textW(it.name, fs) + fs * 1.2);
      const total = widths.reduce((a, b) => a + b, 0);
      const y = legendPos === "top" ? area.y + fs * 0.7 : area.y + area.h - fs * 0.7;
      let x = area.x + Math.max(0, (area.w - total) / 2);
      legendItems.forEach((it, i) => {
        parts.push(<LegendEntry key={`lg${i}`} x={x} y={y} sw={sw} fs={fs} ink={ink} item={it} />);
        x += widths[i]!;
      });
      area = legendPos === "top" ? { ...area, y: area.y + fs * 1.8, h: area.h - fs * 1.8 } : { ...area, h: area.h - fs * 1.8 };
    }
  }

  if (isPie) parts.push(<Pie key="pie" data={data} area={area} colors={sliceColors} doughnut={chart.type === "doughnut"} fs={fs} labels={chart.labels ?? false} format={chart.number_format} />);
  else if (chart.type === "scatter") parts.push(<Scatter key="sc" chart={chart} data={data} area={area} colors={colors} fs={fs} ink={ink} muted={muted} grid={grid} />);
  else parts.push(<Cartesian key="xy" chart={chart} data={data} area={area} colors={colors} fs={fs} ink={ink} muted={muted} grid={grid} />);

  return (
    <svg className="sp-chart" width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label={chart.title ?? `${chart.type} chart`} style={{ fontFamily: font }} data-chart-type={chart.type}>
      {parts}
    </svg>
  );
}

function LegendEntry({ x, y, sw, fs, ink, item }: { x: number; y: number; sw: number; fs: number; ink: string; item: { name: string; color: string; line?: boolean } }) {
  return (
    <g className="sp-legend">
      {item.line ? <line x1={x} x2={x + sw} y1={y} y2={y} stroke={item.color} strokeWidth={Math.max(1.5, fs * 0.2)} /> : <rect x={x} y={y - sw / 2} width={sw} height={sw} fill={item.color} />}
      <text x={x + sw + fs * 0.4} y={y} dominantBaseline="central" fontSize={fs} fill={ink}>
        {item.name}
      </text>
    </g>
  );
}

interface PlotProps {
  chart: Chart;
  data: ChartData;
  area: Box;
  colors: string[];
  fs: number;
  ink: string;
  muted: string;
  grid: string;
}

function extent(values: (number | null)[]): [number, number] {
  const vs = values.filter((v): v is number => v !== null);
  return vs.length ? [Math.min(...vs), Math.max(...vs)] : [0, 1];
}

function Cartesian({ chart, data, area, colors, fs, ink, muted, grid }: PlotProps) {
  const horizontal = chart.type === "bar";
  const stacking = chart.stacked ?? "none";
  const percent = stacking === "percent";
  const stacked = stacking !== "none";
  const n = data.categories.length;
  const primary = data.series.filter((s) => s.axis === "primary");
  const secondary = data.series.filter((s) => s.axis === "secondary");
  const isArea = chart.type === "area";
  const bars = data.series.filter((s) => s.type === "bar");
  const fmt = (v: number) => (percent ? `${formatNumber(v)}%` : formatNumber(v, chart.number_format));

  // In a stacked chart the bar (or area) series stack; lines of a combo chart stay on their own values.
  const stackable = isArea ? primary : bars.filter((s) => s.axis === "primary");
  const stacks = stacked ? stack(stackable, n, percent) : null;
  const stackOf = (s: ChartSeriesData) => (stacks ? stacks[stackable.indexOf(s)] : undefined);

  const primaryValues = stacks ? [...stacks.flat().flat(), ...primary.filter((s) => !stackable.includes(s)).flatMap((s) => s.values)] : primary.flatMap((s) => s.values);
  const [lo, hi] = extent(primaryValues);
  const zero = bars.length > 0 || isArea || stacked;
  const scale: Scale = percent ? { min: 0, max: 100, step: 20, ticks: [0, 20, 40, 60, 80, 100] } : niceScale(lo, hi, 5, zero);
  const scale2 = secondary.length ? niceScale(...extent(secondary.flatMap((s) => s.values)), 5, false) : null;
  const scaleFor = (s: ChartSeriesData) => (s.axis === "secondary" && scale2 ? scale2 : scale);

  const tickLabels = scale.ticks.map(fmt);
  const valueLabelW = Math.max(...tickLabels.map((t) => textW(t, fs))) + fs * 0.6;
  const catLabelW = Math.min(area.w * 0.3, Math.max(...data.categories.map((c) => textW(c, fs))) + fs * 0.6);
  const leftTitle = horizontal ? chart.x_title : chart.y_title;
  const bottomTitle = horizontal ? chart.y_title : chart.x_title;
  const left = (horizontal ? catLabelW : valueLabelW) + (leftTitle ? fs * 1.5 : 0);
  const right = scale2 ? Math.max(...scale2.ticks.map((t) => textW(formatNumber(t, chart.number_format), fs))) + fs * 0.6 : fs * 0.5;
  const bottom = fs * 1.7 + (bottomTitle ? fs * 1.4 : 0);
  const top = chart.labels ? fs * 1.3 : fs * 0.5;
  const plot: Box = { x: area.x + left, y: area.y + top, w: Math.max(1, area.w - left - right), h: Math.max(1, area.h - top - bottom) };

  // Value axis runs along `vLen`; category axis along `cLen`.
  const vPos = (v: number, sc: Scale = scale) => (horizontal ? project(sc, v, plot.x, plot.x + plot.w) : project(sc, v, plot.y + plot.h, plot.y));
  const band = (horizontal ? plot.h : plot.w) / Math.max(1, n);
  const cStart = (c: number) => (horizontal ? plot.y + c * band : plot.x + c * band);
  const cMid = (c: number) => cStart(c) + band / 2;
  const out: ReactNode[] = [];

  scale.ticks.forEach((t, i) => {
    const p = vPos(t);
    out.push(
      horizontal ? <line key={`g${i}`} className="sp-grid" x1={p} x2={p} y1={plot.y} y2={plot.y + plot.h} stroke={grid} strokeWidth={1} /> : <line key={`g${i}`} className="sp-grid" x1={plot.x} x2={plot.x + plot.w} y1={p} y2={p} stroke={grid} strokeWidth={1} />,
      horizontal ? (
        <text key={`t${i}`} className="sp-tick" x={p} y={plot.y + plot.h + fs * 1.1} textAnchor="middle" fontSize={fs} fill={muted}>
          {tickLabels[i]}
        </text>
      ) : (
        <text key={`t${i}`} className="sp-tick" x={plot.x - fs * 0.4} y={p} textAnchor="end" dominantBaseline="central" fontSize={fs} fill={muted}>
          {tickLabels[i]}
        </text>
      ),
    );
  });
  if (scale2 && !horizontal) {
    scale2.ticks.forEach((t, i) =>
      out.push(
        <text key={`t2${i}`} x={plot.x + plot.w + fs * 0.4} y={vPos(t, scale2)} dominantBaseline="central" fontSize={fs} fill={muted}>
          {formatNumber(t, chart.number_format)}
        </text>,
      ),
    );
  }
  const every = Math.max(1, Math.ceil((n * (horizontal ? fs * 1.3 : Math.min(...data.categories.map((c) => textW(c, fs))) + fs)) / (horizontal ? plot.h : plot.w)));
  data.categories.forEach((c, i) => {
    if (i % every !== 0) return;
    out.push(
      horizontal ? (
        <text key={`c${i}`} className="sp-cat" x={plot.x - fs * 0.4} y={cMid(i)} textAnchor="end" dominantBaseline="central" fontSize={fs} fill={ink}>
          {truncate(c, catLabelW, fs)}
        </text>
      ) : (
        <text key={`c${i}`} className="sp-cat" x={cMid(i)} y={plot.y + plot.h + fs * 1.2} textAnchor="middle" fontSize={fs} fill={ink}>
          {truncate(c, band * every, fs)}
        </text>
      ),
    );
  });

  const base = vPos(Math.max(scale.min, Math.min(scale.max, 0)));
  // Bars.
  const groupW = band * 0.72;
  const clustered = bars.length && !stacked ? groupW / bars.length : groupW;
  bars.forEach((s) => {
    const si = data.series.indexOf(s);
    const bi = bars.indexOf(s);
    const st = stackOf(s);
    s.values.forEach((v, c) => {
      if (v === null) return;
      const [from, to] = st ? st[c]! : [0, v];
      const sc = scaleFor(s);
      const a = st ? vPos(from, sc) : s.axis === "secondary" ? vPos(Math.max(sc.min, 0), sc) : base;
      const b = vPos(to, sc);
      const off = cStart(c) + (band - groupW) / 2 + (stacked ? 0 : bi * clustered);
      const thick = Math.max(1, clustered - (stacked ? 0 : Math.min(2, clustered * 0.08)));
      const rect = horizontal ? { x: Math.min(a, b), y: off, width: Math.abs(b - a), height: thick } : { x: off, y: Math.min(a, b), width: thick, height: Math.abs(b - a) };
      out.push(<rect key={`b${si}-${c}`} className="sp-bar" data-series={s.name} {...rect} fill={colors[si]} />);
      if (chart.labels) {
        const label = percent ? `${formatNumber(to - from)}%` : formatNumber(v, chart.number_format);
        if (stacked) {
          if (Math.abs(b - a) > fs * 1.2)
            out.push(
              <text key={`l${si}-${c}`} className="sp-label" x={horizontal ? (a + b) / 2 : off + thick / 2} y={horizontal ? off + thick / 2 : (a + b) / 2} textAnchor="middle" dominantBaseline="central" fontSize={fs * 0.9} fill="#ffffff">
                {label}
              </text>,
            );
        } else if (thick > textW(label, fs * 0.9) * (horizontal ? 0 : 0.8) || horizontal) {
          out.push(
            horizontal ? (
              <text key={`l${si}-${c}`} className="sp-label" x={Math.max(a, b) + fs * 0.3} y={off + thick / 2} dominantBaseline="central" fontSize={fs * 0.9} fill={ink}>
                {label}
              </text>
            ) : (
              <text key={`l${si}-${c}`} className="sp-label" x={off + thick / 2} y={Math.min(a, b) - fs * 0.35} textAnchor="middle" fontSize={fs * 0.9} fill={ink}>
                {label}
              </text>
            ),
          );
        }
      }
    });
  });

  // Lines and areas.
  data.series.forEach((s, si) => {
    if (s.type !== "line") return;
    const st = stackOf(s);
    const sc = scaleFor(s);
    const pts = s.values.map((v, c) => (v === null ? null : { x: cMid(c), y: vPos(st ? st[c]![1] : v, sc), y0: vPos(st ? st[c]![0] : Math.max(sc.min, Math.min(sc.max, 0)), sc), v }));
    const solid = pts.filter((p): p is NonNullable<typeof p> => p !== null);
    if (solid.length === 0) return;
    const path = solid.map((p, i) => `${i ? "L" : "M"}${p.x.toFixed(1)},${p.y.toFixed(1)}`).join("");
    if (isArea) {
      const back = [...solid].reverse().map((p) => `L${p.x.toFixed(1)},${p.y0.toFixed(1)}`).join("");
      out.push(<path key={`a${si}`} className="sp-area" d={`${path}${back}Z`} fill={colors[si]} fillOpacity={stacked ? 0.85 : 0.35} />);
    }
    out.push(<path key={`p${si}`} className="sp-line" data-series={s.name} d={path} fill="none" stroke={colors[si]} strokeWidth={Math.max(1.5, fs * 0.2)} strokeLinejoin="round" strokeLinecap="round" />);
    if (!isArea) solid.forEach((p, i) => out.push(<circle key={`d${si}-${i}`} cx={p.x} cy={p.y} r={Math.max(1.5, fs * 0.28)} fill={colors[si]} />));
    if (chart.labels && !stacked)
      solid.forEach((p, i) =>
        out.push(
          <text key={`ll${si}-${i}`} className="sp-label" x={p.x} y={p.y - fs * 0.6} textAnchor="middle" fontSize={fs * 0.9} fill={ink}>
            {formatNumber(p.v, chart.number_format)}
          </text>,
        ),
      );
  });

  // Axis lines on top of the marks.
  out.push(
    horizontal ? <line key="axis" className="sp-axis" x1={base} x2={base} y1={plot.y} y2={plot.y + plot.h} stroke={muted} strokeWidth={1} /> : <line key="axis" className="sp-axis" x1={plot.x} x2={plot.x + plot.w} y1={base} y2={base} stroke={muted} strokeWidth={1} />,
  );
  if (leftTitle)
    out.push(
      <text key="lt" transform={`translate(${area.x + fs * 0.9},${plot.y + plot.h / 2}) rotate(-90)`} textAnchor="middle" fontSize={fs} fill={muted}>
        {leftTitle}
      </text>,
    );
  if (bottomTitle)
    out.push(
      <text key="bt" x={plot.x + plot.w / 2} y={area.y + area.h - fs * 0.3} textAnchor="middle" fontSize={fs} fill={muted}>
        {bottomTitle}
      </text>,
    );
  return <g>{out}</g>;
}

function Scatter({ chart, data, area, colors, fs, ink, muted, grid }: PlotProps) {
  // The first numeric series is x; every other series is plotted against it.
  const [xs, ...ys] = data.series;
  if (!xs || ys.length === 0) return null;
  const sx = niceScale(...extent(xs.values), 5, false);
  const sy = niceScale(...extent(ys.flatMap((s) => s.values)), 5, false);
  const yLabels = sy.ticks.map((t) => formatNumber(t, chart.number_format));
  const left = Math.max(...yLabels.map((t) => textW(t, fs))) + fs * 0.6 + (chart.y_title ? fs * 1.5 : 0);
  const bottom = fs * 1.7 + (chart.x_title ? fs * 1.4 : 0);
  const plot: Box = { x: area.x + left, y: area.y + fs * 0.5, w: Math.max(1, area.w - left - fs), h: Math.max(1, area.h - bottom - fs * 0.5) };
  const out: ReactNode[] = [];
  sy.ticks.forEach((t, i) => {
    const y = project(sy, t, plot.y + plot.h, plot.y);
    out.push(
      <line key={`gy${i}`} className="sp-grid" x1={plot.x} x2={plot.x + plot.w} y1={y} y2={y} stroke={grid} />,
      <text key={`ty${i}`} x={plot.x - fs * 0.4} y={y} textAnchor="end" dominantBaseline="central" fontSize={fs} fill={muted}>
        {yLabels[i]}
      </text>,
    );
  });
  sx.ticks.forEach((t, i) => {
    const x = project(sx, t, plot.x, plot.x + plot.w);
    out.push(
      <line key={`gx${i}`} className="sp-grid" x1={x} x2={x} y1={plot.y} y2={plot.y + plot.h} stroke={grid} />,
      <text key={`tx${i}`} x={x} y={plot.y + plot.h + fs * 1.2} textAnchor="middle" fontSize={fs} fill={muted}>
        {formatNumber(t)}
      </text>,
    );
  });
  ys.forEach((s, si) =>
    s.values.forEach((v, i) => {
      const x = xs.values[i];
      if (v === null || x === null || x === undefined) return;
      out.push(<circle key={`pt${si}-${i}`} className="sp-point" cx={project(sx, x, plot.x, plot.x + plot.w)} cy={project(sy, v, plot.y + plot.h, plot.y)} r={Math.max(2, fs * 0.4)} fill={colors[si + 1]} fillOpacity={0.85} />);
      if (chart.labels && data.categories[i])
        out.push(
          <text key={`pl${si}-${i}`} x={project(sx, x, plot.x, plot.x + plot.w) + fs * 0.6} y={project(sy, v, plot.y + plot.h, plot.y)} dominantBaseline="central" fontSize={fs * 0.9} fill={ink}>
            {data.categories[i]}
          </text>,
        );
    }),
  );
  out.push(<line key="ax" className="sp-axis" x1={plot.x} x2={plot.x + plot.w} y1={plot.y + plot.h} y2={plot.y + plot.h} stroke={muted} />, <line key="ay" className="sp-axis" x1={plot.x} x2={plot.x} y1={plot.y} y2={plot.y + plot.h} stroke={muted} />);
  const xTitle = chart.x_title ?? xs.name;
  out.push(
    <text key="xt" x={plot.x + plot.w / 2} y={area.y + area.h - fs * 0.3} textAnchor="middle" fontSize={fs} fill={muted}>
      {xTitle}
    </text>,
  );
  if (chart.y_title)
    out.push(
      <text key="yt" transform={`translate(${area.x + fs * 0.9},${plot.y + plot.h / 2}) rotate(-90)`} textAnchor="middle" fontSize={fs} fill={muted}>
        {chart.y_title}
      </text>,
    );
  return <g>{out}</g>;
}

function Pie({ data, area, colors, doughnut, fs, labels, format }: { data: ChartData; area: Box; colors: string[]; doughnut: boolean; fs: number; labels: boolean; format?: string }) {
  const s = data.series[0];
  if (!s) return null;
  const r = Math.max(1, Math.min(area.w, area.h) / 2 - fs * 0.5);
  const cx = area.x + area.w / 2;
  const cy = area.y + area.h / 2;
  const inner = doughnut ? r * 0.55 : 0;
  const at = (a: number, rad: number) => [cx + Math.sin(a) * rad, cy - Math.cos(a) * rad] as const;
  const total = s.values.reduce<number>((n, v) => n + Math.max(0, v ?? 0), 0);
  return (
    <g>
      {pieAngles(s.values).map(([a, b], i) => {
        if (b - a <= 0) return null;
        const large = b - a > Math.PI ? 1 : 0;
        const full = b - a >= Math.PI * 2 - 1e-9;
        const [x1, y1] = at(a, r);
        const [x2, y2] = at(full ? a + Math.PI * 2 - 1e-4 : b, r);
        const [x3, y3] = at(full ? a + Math.PI * 2 - 1e-4 : b, inner);
        const [x4, y4] = at(a, inner);
        const d = inner > 0 ? `M${x1},${y1}A${r},${r} 0 ${large} 1 ${x2},${y2}L${x3},${y3}A${inner},${inner} 0 ${large} 0 ${x4},${y4}Z` : `M${cx},${cy}L${x1},${y1}A${r},${r} 0 ${large} 1 ${x2},${y2}Z`;
        const [lx, ly] = at((a + b) / 2, inner > 0 ? (r + inner) / 2 : r * 0.62);
        const v = s.values[i] ?? 0;
        return (
          <g key={i}>
            <path className="sp-slice" d={d} fill={colors[i]} stroke="#ffffff" strokeWidth={1} />
            {labels && b - a > 0.3 ? (
              <text x={lx} y={ly} textAnchor="middle" dominantBaseline="central" fontSize={fs * 0.9} fill="#ffffff">
                {format ? formatNumber(v, format) : `${Math.round((v / (total || 1)) * 100)}%`}
              </text>
            ) : null}
          </g>
        );
      })}
    </g>
  );
}
