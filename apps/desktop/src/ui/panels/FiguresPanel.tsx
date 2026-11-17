import { useMemo, useState } from "react";
import type { FigureGroup, FigureView, RunId, RunView, SessionId } from "../../ipc";
import { useFigureSrc } from "../../state/figureCache";
import { groupFigures } from "../../state/figures";
import type { FigureEntry } from "../../state/figures";
import { useChat } from "../../state/chat";
import { useRuns } from "../../state/runs";
import type { FigureSelection } from "../../state/runs";
import { useActiveSession, useSessionInfo } from "../../state/sessions";
import { formatTime, shortArgs } from "../common/format";
import { focusChatInput } from "../workbench/controller";

const NO_GROUPS: FigureGroup[] = [];
const NO_PINS: FigureSelection[] = [];

function FigureImage({ session, file, alt }: { session: SessionId; file: FigureView; alt: string }) {
  const src = useFigureSrc(session, file);
  if (!src) return <div className="empty">Loading…</div>;
  if (file.format === "pdf") {
    return (
      <object data={src} type="application/pdf" aria-label={alt}>
        <div className="empty">This figure is a PDF and cannot be previewed here.</div>
      </object>
    );
  }
  return <img src={src} alt={alt} decoding="async" />;
}

/** One figure: the PNG (or the best format there is), with the other formats one click away in the caption row. */
function FigureFrame({ session, entry, label }: { session: SessionId; entry: FigureEntry; label: string }) {
  const [format, setFormat] = useState<FigureView["format"] | null>(null);
  const file = entry.formats.find((f) => f.format === format) ?? entry.primary;
  const meta = entry.primary;
  return (
    <div className="figure-frame" data-testid="figure-frame">
      <FigureImage session={session} file={file} alt={meta.sidecar?.title ?? entry.name} />
      <div className="figure-caption">
        <span className="truncate grow">
          {label} · {meta.sidecar?.script ?? meta.script} {shortArgs(meta.sidecar?.args ?? [])}
        </span>
        {entry.formats.length > 1
          ? entry.formats.map((f) => (
              <button
                key={f.format}
                type="button"
                className={`figure-format${f === file ? " active" : ""}`}
                aria-pressed={f === file}
                title={f.path}
                onClick={() => setFormat(f.format)}
                data-testid="figure-format"
              >
                {f.format}
              </button>
            ))
          : null}
      </div>
    </div>
  );
}

interface RunFigures {
  run: RunView;
  entries: FigureEntry[];
}

export function FiguresPanel() {
  const session = useActiveSession();
  const info = useSessionInfo(session);
  const groups = useRuns((s) => (session ? (s.figures[session] ?? NO_GROUPS) : NO_GROUPS));
  const active = useRuns((s) => (session ? (s.activeFigure[session] ?? null) : null));
  const pinned = useRuns((s) => (session ? (s.pinned[session] ?? NO_PINS) : NO_PINS));
  const compare = useRuns((s) => (session ? (s.compare[session] ?? null) : null));

  // One entry per figure and run (formats folded together), newest run first.
  const runs = useMemo<RunFigures[]>(() => groups.map((g) => ({ run: g.run, entries: groupFigures(g.figures) })), [groups]);
  // One tab per figure name across runs.
  const names = useMemo(() => {
    const seen: string[] = [];
    for (const r of runs) for (const e of r.entries) if (!seen.includes(e.name)) seen.push(e.name);
    return seen;
  }, [runs]);

  if (!session) return <div className="empty">No session</div>;
  if (names.length === 0) return <div className="empty">Figures appear here as runs produce them.</div>;

  const name = active && names.includes(active.name) ? active.name : names[0];
  const entryOf = (r: RunFigures | undefined) => r?.entries.find((e) => e.name === name);
  const history = runs.filter((r) => entryOf(r) !== undefined);
  const selectedRun: RunId | null = active && history.some((r) => r.run.meta.run_id === active.run_id) ? active.run_id : (history[0]?.run.meta.run_id ?? null);
  const figure = entryOf(history.find((r) => r.run.meta.run_id === selectedRun));
  const compareFigure = compare ? entryOf(history.find((r) => r.run.meta.run_id === compare && r.run.meta.run_id !== selectedRun)) : undefined;
  const isPinned = figure ? pinned.some((p) => p.name === figure.name && p.run_id === figure.primary.run_id) : false;

  const select = (n: string) => {
    const latest = runs.find((r) => r.entries.some((e) => e.name === n));
    if (latest) useRuns.getState().setActiveFigure(session, { name: n, run_id: latest.run.meta.run_id });
  };
  const sendToAgent = () => {
    if (!figure || !info) return;
    useChat.getState().addAttachment(info.active_conversation, { kind: "figure", path: figure.primary.path });
    focusChatInput();
  };

  return (
    <div className="panel" data-testid="figures-panel">
      <div className="panel-toolbar">
        <div className="figure-tabs" role="tablist">
          {names.map((n) => (
            <button key={n} type="button" role="tab" aria-selected={n === name} className={`figure-tab small${n === name ? " active" : ""}`} onClick={() => select(n)} data-testid="figure-tab">
              {n}
              {pinned.some((p) => p.name === n) ? " 📌" : ""}
            </button>
          ))}
        </div>
        <span className="grow" />
        <label className="row small muted">
          run
          <select aria-label="Figure run" value={selectedRun ?? ""} onChange={(e) => name && useRuns.getState().setActiveFigure(session, { name, run_id: e.target.value })}>
            {history.map((r, i) => (
              <option key={r.run.meta.run_id} value={r.run.meta.run_id}>
                {i === 0 ? "latest · " : ""}
                {formatTime(r.run.meta.started_at)} {shortArgs(r.run.meta.args)}
              </option>
            ))}
          </select>
        </label>
        <label className="row small muted">
          compare
          <select aria-label="Compare with run" value={compare ?? ""} onChange={(e) => useRuns.getState().setCompare(session, e.target.value || null)}>
            <option value="">none</option>
            {history
              .filter((r) => r.run.meta.run_id !== selectedRun)
              .map((r) => (
                <option key={r.run.meta.run_id} value={r.run.meta.run_id}>
                  {formatTime(r.run.meta.started_at)} {shortArgs(r.run.meta.args)}
                </option>
              ))}
          </select>
        </label>
        <button
          type="button"
          className={`small${isPinned ? " primary" : ""}`}
          onClick={() => figure && useRuns.getState().togglePin(session, { name: figure.name, run_id: figure.primary.run_id })}
          aria-pressed={isPinned}
          title="Keep this run selected when new runs arrive"
        >
          {isPinned ? "Pinned" : "Pin"}
        </button>
        <button type="button" className="small" onClick={sendToAgent} disabled={!figure}>
          Send to agent
        </button>
      </div>
      <div className="panel-body">
        <div className="figure-view">
          {figure ? <FigureFrame key={figure.primary.path} session={session} entry={figure} label={formatTime(figure.primary.created_at)} /> : null}
          {compareFigure ? <FigureFrame key={compareFigure.primary.path} session={session} entry={compareFigure} label={`compare · ${formatTime(compareFigure.primary.created_at)}`} /> : null}
        </div>
      </div>
    </div>
  );
}
