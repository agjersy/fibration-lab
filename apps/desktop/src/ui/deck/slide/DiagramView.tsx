// Approximate drawings of the four diagram families (PROTOCOL 2.5). The kernel owns the exact layout
// (PROTOCOL 5.3) and will supply geometry through `deck_get`; until then the preview lays the content
// out with simple rules so the structure is legible.
import type { CSSProperties, ReactNode } from "react";
import { plainText } from "../../../deck/model";
import type { DNode, Diagram, HNode, Theme } from "../../../deck/types";
import { contrastOn, fontStack, mix, resolveColor, seriesColor } from "./theme";

interface DiagramViewProps {
  diagram: Diagram;
  theme: Theme;
  width: number;
  height: number;
  fontPx: number;
}

interface Ctx {
  theme: Theme;
  w: number;
  h: number;
  fs: number;
  ink: string;
  muted: string;
  accent: string;
  soft: string;
}

/** Parses the dates the schema accepts (`2026-03`, `2026-03-15`, `2026-Q1`, `2026`) to a UTC timestamp. */
export function parseDate(value: string, end = false): number | null {
  let m = /^(\d{4})-Q([1-4])$/i.exec(value);
  if (m) return Date.UTC(Number(m[1]), (Number(m[2]) - 1) * 3 + (end ? 3 : 0), 1);
  m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (m) return Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]) + (end ? 1 : 0));
  m = /^(\d{4})-(\d{2})$/.exec(value);
  if (m) return Date.UTC(Number(m[1]), Number(m[2]) - 1 + (end ? 1 : 0), 1);
  m = /^(\d{4})$/.exec(value);
  if (m) return Date.UTC(Number(m[1]) + (end ? 1 : 0), 0, 1);
  return null;
}

/** Axis ticks between two timestamps for a unit, with labels. */
export function timeTicks(from: number, to: number, unit: "day" | "week" | "month" | "quarter" | "year"): { at: number; label: string }[] {
  const out: { at: number; label: string }[] = [];
  const d = new Date(from);
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  if (unit === "year") d.setUTCMonth(0, 1);
  else if (unit === "quarter") d.setUTCMonth(Math.floor(d.getUTCMonth() / 3) * 3, 1);
  else if (unit === "month") d.setUTCDate(1);
  d.setUTCHours(0, 0, 0, 0);
  for (let guard = 0; d.getTime() <= to && guard < 400; guard++) {
    const at = d.getTime();
    if (at >= from) {
      const y = d.getUTCFullYear();
      const label = unit === "year" ? String(y) : unit === "quarter" ? `Q${Math.floor(d.getUTCMonth() / 3) + 1} ${y}` : unit === "month" ? `${months[d.getUTCMonth()]} ${String(y).slice(2)}` : `${d.getUTCDate()} ${months[d.getUTCMonth()]}`;
      out.push({ at, label });
    }
    if (unit === "year") d.setUTCFullYear(d.getUTCFullYear() + 1);
    else if (unit === "quarter") d.setUTCMonth(d.getUTCMonth() + 3);
    else if (unit === "month") d.setUTCMonth(d.getUTCMonth() + 1);
    else d.setUTCDate(d.getUTCDate() + (unit === "week" ? 7 : 1));
  }
  return out;
}

/** Tidy positions of a forest: x in leaf units (parents centred over children), depth per node. */
export function treeLayout(nodes: Pick<HNode, "id" | "parent">[]): { pos: Map<string, { x: number; depth: number }>; leaves: number; depth: number } {
  const ids = new Set(nodes.map((n) => n.id));
  const kids = new Map<string, string[]>();
  const roots: string[] = [];
  for (const n of nodes) {
    if (n.parent && ids.has(n.parent) && n.parent !== n.id) kids.set(n.parent, [...(kids.get(n.parent) ?? []), n.id]);
    else roots.push(n.id);
  }
  const pos = new Map<string, { x: number; depth: number }>();
  let leaf = 0;
  let maxDepth = 0;
  const place = (id: string, depth: number, seen: Set<string>): number => {
    if (seen.has(id)) return leaf;
    seen.add(id);
    maxDepth = Math.max(maxDepth, depth);
    const cs = (kids.get(id) ?? []).filter((c) => !seen.has(c));
    const x = cs.length === 0 ? leaf++ : cs.map((c) => place(c, depth + 1, seen)).reduce((a, b) => a + b, 0) / cs.length;
    pos.set(id, { x, depth });
    return x;
  };
  const seen = new Set<string>();
  for (const r of roots) place(r, 0, seen);
  return { pos, leaves: Math.max(1, leaf), depth: maxDepth + 1 };
}

const abs = (x: number, y: number, w: number, h: number, extra: CSSProperties = {}): CSSProperties => ({ position: "absolute", left: x, top: y, width: w, height: h, ...extra });
const center: CSSProperties = { display: "flex", alignItems: "center", justifyContent: "center", textAlign: "center", overflow: "hidden" };

export function DiagramView({ diagram, theme, width, height, fontPx }: DiagramViewProps) {
  const ink = resolveColor(theme, "dk1");
  const accent = resolveColor(theme, "accent1");
  const c: Ctx = { theme, w: width, h: height, fs: fontPx, ink, muted: mix(ink, "#ffffff", 0.4), accent, soft: mix(accent, "#ffffff", 0.86) };
  let body: ReactNode;
  if (diagram.family === "process") body = process(c, diagram.variant, diagram.nodes, diagram.edges ?? []);
  else if (diagram.family === "timeline") body = timeline(c, diagram);
  else if (diagram.family === "matrix") body = matrix(c, diagram);
  else body = hierarchy(c, diagram.nodes);
  return (
    <div className="sp-diagram" data-family={diagram.family} data-variant={diagram.variant} style={{ position: "relative", width, height, fontFamily: fontStack(theme.fonts.minor, theme), fontSize: c.fs, color: ink, lineHeight: 1.2 }}>
      {body}
    </div>
  );
}

function Arrow({ c, x1, y1, x2, y2, label }: { c: Ctx; x1: number; y1: number; x2: number; y2: number; label?: string }) {
  const a = Math.atan2(y2 - y1, x2 - x1);
  const s = Math.max(4, c.fs * 0.5);
  const head = `${x2},${y2} ${x2 - s * Math.cos(a - 0.45)},${y2 - s * Math.sin(a - 0.45)} ${x2 - s * Math.cos(a + 0.45)},${y2 - s * Math.sin(a + 0.45)}`;
  return (
    <g>
      <line x1={x1} y1={y1} x2={x2} y2={y2} stroke={c.muted} strokeWidth={Math.max(1, c.fs * 0.1)} />
      <polygon points={head} fill={c.muted} />
      {label ? (
        <text x={(x1 + x2) / 2} y={(y1 + y2) / 2 - c.fs * 0.3} textAnchor="middle" fontSize={c.fs * 0.8} fill={c.muted}>
          {label}
        </text>
      ) : null}
    </g>
  );
}

const overlay = (c: Ctx, children: ReactNode) => (
  <svg width={c.w} height={c.h} style={{ position: "absolute", left: 0, top: 0, pointerEvents: "none" }}>
    {children}
  </svg>
);

function process(c: Ctx, variant: "chevrons" | "steps" | "flowchart", nodes: DNode[], edges: { from: string; to: string; label?: string }[]): ReactNode {
  const n = nodes.length;
  if (n === 0) return null;
  const hasDetail = nodes.some((x) => x.detail);
  if (variant === "chevrons") {
    const gap = c.fs * 0.3;
    const w = (c.w - gap * (n - 1)) / n;
    const h = Math.min(c.h * (hasDetail ? 0.32 : 0.5), w * 0.6);
    const top = hasDetail ? Math.max(0, c.h * 0.42 - h) : (c.h - h) / 2;
    const notch = Math.min(h * 0.35, w * 0.18);
    return nodes.map((node, i) => {
      const fill = seriesColor(c.theme, 0);
      const shade = mix(fill, "#ffffff", (i / Math.max(1, n)) * 0.45);
      const clip = `polygon(0 0, calc(100% - ${notch}px) 0, 100% 50%, calc(100% - ${notch}px) 100%, 0 100%${i === 0 ? "" : `, ${notch}px 50%`})`;
      return (
        <div key={node.id} className="sp-node" data-node={node.id}>
          <div style={abs(i * (w + gap), top, w, h, { ...center, background: shade, color: contrastOn(shade), clipPath: clip, fontWeight: 700, padding: `0 ${notch + c.fs * 0.2}px`, boxSizing: "border-box" })}>{plainText(node.label)}</div>
          {node.detail ? <div style={abs(i * (w + gap), top + h + c.fs * 0.8, w - notch, c.h - top - h - c.fs * 0.8, { fontSize: c.fs * 0.85, color: c.ink, overflow: "hidden", padding: `0 ${c.fs * 0.3}px`, boxSizing: "border-box" })}>{plainText(node.detail)}</div> : null}
        </div>
      );
    });
  }
  if (variant === "steps") {
    const w = c.w / n;
    const r = Math.min(w * 0.22, c.h * 0.16, c.fs * 2.2);
    const cy = hasDetail ? c.h * 0.22 : c.h * 0.35;
    return (
      <>
        {overlay(
          c,
          nodes.slice(1).map((node, i) => <Arrow key={node.id} c={c} x1={i * w + w / 2 + r + 4} y1={cy} x2={(i + 1) * w + w / 2 - r - 4} y2={cy} />),
        )}
        {nodes.map((node, i) => (
          <div key={node.id} className="sp-node" data-node={node.id}>
            <div style={abs(i * w + w / 2 - r, cy - r, 2 * r, 2 * r, { ...center, borderRadius: "50%", background: c.accent, color: contrastOn(c.accent), fontWeight: 700, fontSize: c.fs * 1.2 })}>{i + 1}</div>
            <div style={abs(i * w + c.fs * 0.3, cy + r + c.fs * 0.5, w - c.fs * 0.6, c.h - cy - r - c.fs * 0.5, { textAlign: "center", overflow: "hidden" })}>
              <div style={{ fontWeight: 700 }}>{plainText(node.label)}</div>
              {node.detail ? <div style={{ fontSize: c.fs * 0.85, marginTop: c.fs * 0.3, color: c.muted }}>{plainText(node.detail)}</div> : null}
            </div>
          </div>
        ))}
      </>
    );
  }
  // Flowchart: boxes in rows of up to four, arrows along the edges (or in sequence without edges).
  const perRow = Math.min(n, 4);
  const rows = Math.ceil(n / perRow);
  const cellW = c.w / perRow;
  const cellH = c.h / rows;
  const bw = cellW * 0.72;
  const bh = Math.min(cellH * 0.55, c.fs * 5);
  const boxOf = (i: number) => ({ x: (i % perRow) * cellW + (cellW - bw) / 2, y: Math.floor(i / perRow) * cellH + (cellH - bh) / 2 });
  const index = new Map(nodes.map((x, i) => [x.id, i]));
  const links = edges.length ? edges : nodes.slice(1).map((x, i) => ({ from: nodes[i]!.id, to: x.id }));
  return (
    <>
      {overlay(
        c,
        links.map((e: { from: string; to: string; label?: string }, k) => {
          const a = index.get(e.from);
          const b = index.get(e.to);
          if (a === undefined || b === undefined) return null;
          const p = boxOf(a);
          const q = boxOf(b);
          const sameRow = Math.abs(p.y - q.y) < 1;
          const forward = q.x >= p.x;
          const x1 = sameRow ? (forward ? p.x + bw : p.x) : p.x + bw / 2;
          const y1 = sameRow ? p.y + bh / 2 : q.y > p.y ? p.y + bh : p.y;
          const x2 = sameRow ? (forward ? q.x : q.x + bw) : q.x + bw / 2;
          const y2 = sameRow ? q.y + bh / 2 : q.y > p.y ? q.y : q.y + bh;
          return <Arrow key={k} c={c} x1={x1} y1={y1} x2={x2} y2={y2} label={e.label} />;
        }),
      )}
      {nodes.map((node, i) => {
        const { x, y } = boxOf(i);
        const decision = node.kind === "decision";
        const terminal = node.kind === "start" || node.kind === "end";
        const fill = terminal ? c.accent : c.soft;
        return (
          <div
            key={node.id}
            className="sp-node"
            data-node={node.id}
            style={abs(x, y, bw, bh, {
              ...center,
              background: fill,
              color: contrastOn(fill),
              border: decision || terminal ? undefined : `${Math.max(1, c.fs * 0.08)}px solid ${c.accent}`,
              borderRadius: terminal ? bh / 2 : c.fs * 0.3,
              clipPath: decision ? "polygon(50% 0, 100% 50%, 50% 100%, 0 50%)" : undefined,
              padding: `0 ${decision ? bw * 0.2 : c.fs * 0.4}px`,
              boxSizing: "border-box",
              fontWeight: 600,
            })}
          >
            {plainText(node.label)}
          </div>
        );
      })}
    </>
  );
}

function timeline(c: Ctx, d: Extract<Diagram, { family: "timeline" }>): ReactNode {
  const items = d.items.flatMap((it) => {
    const start = parseDate(it.start);
    if (start === null) return [];
    const end = it.end ? (parseDate(it.end, true) ?? start) : start;
    return [{ it, start, end }];
  });
  if (items.length === 0) return null;
  const lo = (d.axis && parseDate(d.axis.from)) ?? Math.min(...items.map((x) => x.start));
  const hi = (d.axis && parseDate(d.axis.to, true)) ?? Math.max(...items.map((x) => x.end));
  const span = Math.max(1, hi - lo);
  const padX = c.fs * 1.5;
  const X = (t: number) => padX + ((Math.min(hi, Math.max(lo, t)) - lo) / span) * (c.w - 2 * padX);
  const days = span / 86_400_000;
  const unit = d.axis?.unit ?? (days > 900 ? "year" : days > 270 ? "quarter" : days > 60 ? "month" : days > 21 ? "week" : "day");
  const ticks = timeTicks(lo, hi, unit);
  const stroke = Math.max(1, c.fs * 0.12);

  if (d.variant === "milestones") {
    const y = c.h * 0.5;
    const slot = (c.w - 2 * padX) / Math.max(1, items.length);
    const lw = Math.min(c.w * 0.3, Math.max(slot * 1.6, c.fs * 7));
    const r = Math.max(3, c.fs * 0.45);
    return (
      <>
        {overlay(
          c,
          <>
            <line x1={padX} x2={c.w - padX} y1={y} y2={y} stroke={c.muted} strokeWidth={stroke * 1.5} />
            {ticks.map((t) => (
              <g key={t.at}>
                <line x1={X(t.at)} x2={X(t.at)} y1={y - c.fs * 0.35} y2={y + c.fs * 0.35} stroke={c.muted} strokeWidth={stroke} />
                <text x={X(t.at)} y={c.h - c.fs * 0.3} textAnchor="middle" fontSize={c.fs * 0.8} fill={c.muted}>
                  {t.label}
                </text>
              </g>
            ))}
            {items.map(({ it, start }, i) => {
              const up = i % 2 === 0;
              const len = c.h * (i % 4 < 2 ? 0.2 : 0.33) - c.fs;
              return (
                <g key={it.id}>
                  <line x1={X(start)} x2={X(start)} y1={y} y2={up ? y - len : y + len} stroke={c.accent} strokeWidth={stroke} />
                  <circle cx={X(start)} cy={y} r={r} fill={c.accent} stroke="#ffffff" strokeWidth={stroke} />
                </g>
              );
            })}
          </>,
        )}
        {items.map(({ it, start }, i) => {
          const up = i % 2 === 0;
          const len = c.h * (i % 4 < 2 ? 0.2 : 0.33) - c.fs;
          const left = Math.min(c.w - lw, Math.max(0, X(start) - lw / 2));
          const boxH = c.fs * 3.2;
          return (
            <div key={it.id} className="sp-node" data-node={it.id} style={abs(left, up ? y - len - boxH : y + len, lw, boxH, { display: "flex", flexDirection: "column", justifyContent: up ? "flex-end" : "flex-start", textAlign: "center", overflow: "hidden" })}>
              <div style={{ fontWeight: 700 }}>{plainText(it.label)}</div>
              <div style={{ fontSize: c.fs * 0.8, color: c.muted }}>{it.start}</div>
            </div>
          );
        })}
      </>
    );
  }
  // Swimlanes and Gantt: one row per lane (or per item), bars from start to end.
  const lanes = d.variant === "swimlanes" ? (d.lanes?.length ? d.lanes : [...new Set(items.map((x) => x.it.lane ?? ""))]) : items.map((x) => x.it.id);
  const labelW = Math.min(c.w * 0.28, c.fs * 9);
  const axisH = c.fs * 1.8;
  const rowH = Math.min((c.h - axisH) / Math.max(1, lanes.length), c.fs * 3.2);
  const BX = (t: number) => labelW + ((Math.min(hi, Math.max(lo, t)) - lo) / span) * (c.w - labelW - c.fs * 0.5);
  return (
    <>
      {overlay(
        c,
        <>
          {ticks.map((t) => (
            <g key={t.at}>
              <line x1={BX(t.at)} x2={BX(t.at)} y1={axisH} y2={axisH + rowH * lanes.length} stroke={mix(c.ink, "#ffffff", 0.85)} strokeWidth={1} />
              <text x={BX(t.at)} y={c.fs * 1.1} fontSize={c.fs * 0.8} fill={c.muted}>
                {t.label}
              </text>
            </g>
          ))}
          {lanes.map((_, i) => (
            <line key={i} x1={0} x2={c.w} y1={axisH + i * rowH} y2={axisH + i * rowH} stroke={mix(c.ink, "#ffffff", 0.85)} strokeWidth={1} />
          ))}
        </>,
      )}
      {lanes.map((lane, i) => (
        <div key={`lane${i}`} style={abs(0, axisH + i * rowH, labelW - c.fs * 0.4, rowH, { display: "flex", alignItems: "center", fontWeight: 600, overflow: "hidden", fontSize: c.fs * 0.9 })}>
          {d.variant === "swimlanes" ? lane : plainText(items[i]!.it.label)}
        </div>
      ))}
      {items.map(({ it, start, end }, i) => {
        const row = d.variant === "swimlanes" ? Math.max(0, lanes.indexOf(it.lane ?? "")) : i;
        const x = BX(start);
        const w = Math.max(c.fs * 0.8, BX(end) - x);
        const fill = seriesColor(c.theme, d.variant === "swimlanes" ? row : 0);
        return (
          <div key={it.id} className="sp-node" data-node={it.id} style={abs(x, axisH + row * rowH + rowH * 0.2, w, rowH * 0.6, { ...center, background: fill, color: contrastOn(fill), borderRadius: c.fs * 0.25, fontSize: c.fs * 0.8, whiteSpace: "nowrap" })}>
            {d.variant === "swimlanes" ? plainText(it.label) : ""}
          </div>
        );
      })}
    </>
  );
}

function harvey(score: number, size: number, color: string): ReactNode {
  const r = size / 2 - 1;
  const a = (Math.min(4, Math.max(0, score)) / 4) * Math.PI * 2;
  const x = size / 2 + Math.sin(a) * r;
  const y = size / 2 - Math.cos(a) * r;
  return (
    <svg width={size} height={size} className="sp-harvey" data-score={score}>
      <circle cx={size / 2} cy={size / 2} r={r} fill="#ffffff" stroke={color} strokeWidth={1.2} />
      {score >= 4 ? <circle cx={size / 2} cy={size / 2} r={r} fill={color} /> : score > 0 ? <path d={`M${size / 2},${size / 2}L${size / 2},${size / 2 - r}A${r},${r} 0 ${a > Math.PI ? 1 : 0} 1 ${x},${y}Z`} fill={color} /> : null}
    </svg>
  );
}

function matrix(c: Ctx, d: Extract<Diagram, { family: "matrix" }>): ReactNode {
  const line = mix(c.ink, "#ffffff", 0.7);
  if (d.variant === "2x2") {
    const ax = d.axes;
    const m = c.fs * 2.6;
    const size = Math.max(10, Math.min(c.w - m * 1.5, c.h - m * 1.5));
    const x0 = (c.w - size) / 2 + m * 0.4;
    const y0 = (c.h - size - m) / 2;
    return (
      <>
        <div style={abs(x0, y0, size, size, { border: `1px solid ${line}`, boxSizing: "border-box", background: `linear-gradient(${line},${line}) 50% 0/1px 100% no-repeat, linear-gradient(${line},${line}) 0 50%/100% 1px no-repeat, ${c.soft}` })} />
        {ax ? (
          <>
            <div style={abs(x0, y0 + size + c.fs * 0.2, size, c.fs * 1.2, { display: "flex", justifyContent: "space-between", fontSize: c.fs * 0.8, color: c.muted })}>
              <span>{ax.x_low}</span>
              <b style={{ color: c.ink }}>{ax.x}</b>
              <span>{ax.x_high}</span>
            </div>
            <div style={abs(x0 - c.fs * 1.5 - size / 2 + c.fs * 0.6, y0 + size / 2 - c.fs * 0.6, size, c.fs * 1.2, { display: "flex", justifyContent: "space-between", fontSize: c.fs * 0.8, color: c.muted, transform: "rotate(-90deg)" })}>
              <span>{ax.y_low}</span>
              <b style={{ color: c.ink }}>{ax.y}</b>
              <span>{ax.y_high}</span>
            </div>
          </>
        ) : null}
        {d.cells.map((cell, i) => {
          const px = x0 + Math.min(1, Math.max(0, cell.x ?? 0.5)) * size;
          const py = y0 + (1 - Math.min(1, Math.max(0, cell.y ?? 0.5))) * size;
          const r = c.fs * 0.45;
          return (
            <div key={i} className="sp-node">
              <div style={abs(px - r, py - r, 2 * r, 2 * r, { borderRadius: "50%", background: seriesColor(c.theme, 1) })} />
              <div style={abs(px + r + 2, py - c.fs * 0.6, size * 0.4, c.fs * 1.3, { fontSize: c.fs * 0.85, whiteSpace: "nowrap", overflow: "hidden" })}>{plainText(cell.label)}</div>
            </div>
          );
        })}
      </>
    );
  }
  if (d.variant === "kpi-tiles") {
    const n = d.cells.length;
    if (n === 0) return null;
    const cols = n <= 4 ? n : Math.ceil(Math.sqrt(n));
    const rows = Math.ceil(n / cols);
    const gap = c.fs * 0.8;
    const tw = (c.w - gap * (cols - 1)) / cols;
    const th = Math.min((c.h - gap * (rows - 1)) / rows, tw * 0.8);
    const y0 = (c.h - (th * rows + gap * (rows - 1))) / 2;
    return d.cells.map((cell, i) => (
      <div key={i} className="sp-node" style={abs((i % cols) * (tw + gap), y0 + Math.floor(i / cols) * (th + gap), tw, th, { ...center, flexDirection: "column", background: c.soft, borderTop: `${Math.max(2, c.fs * 0.25)}px solid ${seriesColor(c.theme, 0)}`, boxSizing: "border-box", padding: c.fs * 0.4 })}>
        <div style={{ fontSize: Math.min(th * 0.34, c.fs * 2.6), fontWeight: 700, color: c.accent, lineHeight: 1.1 }}>{cell.value ?? ""}</div>
        <div style={{ fontSize: c.fs * 0.9, marginTop: c.fs * 0.3 }}>{plainText(cell.label)}</div>
      </div>
    ));
  }
  // Comparison grid: row and column headers, Harvey balls for scores.
  const rows = d.rows ?? [];
  const cols = d.columns ?? [];
  const nr = Math.max(rows.length, ...d.cells.map((x) => (x.row ?? 0) + 1), 1);
  const nc = Math.max(cols.length, ...d.cells.map((x) => (x.col ?? 0) + 1), 1);
  const headW = rows.length ? Math.min(c.w * 0.3, c.fs * 10) : 0;
  const headH = cols.length ? c.fs * 2.2 : 0;
  const cw = (c.w - headW) / nc;
  const ch = Math.min((c.h - headH) / nr, c.fs * 4);
  return (
    <>
      {cols.map((name, j) => (
        <div key={`c${j}`} style={abs(headW + j * cw, 0, cw, headH, { ...center, background: c.accent, color: contrastOn(c.accent), fontWeight: 700, borderLeft: "1px solid #ffffff", boxSizing: "border-box" })}>
          {name}
        </div>
      ))}
      {rows.map((name, i) => (
        <div key={`r${i}`} style={abs(0, headH + i * ch, headW, ch, { display: "flex", alignItems: "center", fontWeight: 600, borderBottom: `1px solid ${line}`, boxSizing: "border-box", overflow: "hidden" })}>
          {name}
        </div>
      ))}
      {d.cells.map((cell, k) => (
        <div key={k} className="sp-node" style={abs(headW + (cell.col ?? 0) * cw, headH + (cell.row ?? 0) * ch, cw, ch, { ...center, gap: c.fs * 0.4, borderBottom: `1px solid ${line}`, boxSizing: "border-box", fontSize: c.fs * 0.9 })}>
          {cell.score !== undefined ? harvey(cell.score, Math.min(ch * 0.6, c.fs * 1.6), c.accent) : null}
          <span>{cell.value ?? plainText(cell.label)}</span>
        </div>
      ))}
    </>
  );
}

function hierarchy(c: Ctx, nodes: HNode[]): ReactNode {
  if (nodes.length === 0) return null;
  const { pos, leaves, depth } = treeLayout(nodes);
  const colW = c.w / leaves;
  const rowH = c.h / depth;
  const bw = Math.min(colW * 0.86, c.fs * 11);
  const bh = Math.min(rowH * 0.6, c.fs * 3.6);
  const at = (id: string) => {
    const p = pos.get(id)!;
    return { x: (p.x + 0.5) * colW, y: p.depth * rowH + (rowH - bh) / 2 };
  };
  return (
    <>
      {overlay(
        c,
        nodes.map((n) => {
          if (!n.parent || !pos.has(n.parent) || !pos.has(n.id)) return null;
          const a = at(n.parent);
          const b = at(n.id);
          const mid = (a.y + bh + b.y) / 2;
          return <path key={n.id} className="sp-link" d={`M${a.x},${a.y + bh}V${mid}H${b.x}V${b.y}`} fill="none" stroke={c.muted} strokeWidth={Math.max(1, c.fs * 0.1)} />;
        }),
      )}
      {nodes.map((n) => {
        if (!pos.has(n.id)) return null;
        const p = at(n.id);
        const root = pos.get(n.id)!.depth === 0;
        const fill = root ? c.accent : c.soft;
        return (
          <div key={n.id} className="sp-node" data-node={n.id} style={abs(p.x - bw / 2, p.y, bw, bh, { ...center, flexDirection: "column", background: fill, color: contrastOn(fill), border: root ? undefined : `1px solid ${c.accent}`, borderRadius: c.fs * 0.3, boxSizing: "border-box", padding: `0 ${c.fs * 0.3}px`, fontSize: c.fs * 0.9 })}>
            <div style={{ fontWeight: 700 }}>{plainText(n.label)}</div>
            {n.value ? <div style={{ fontSize: c.fs * 0.85 }}>{n.value}</div> : n.detail ? <div style={{ fontSize: c.fs * 0.8, opacity: 0.8 }}>{plainText(n.detail)}</div> : null}
          </div>
        );
      })}
    </>
  );
}
