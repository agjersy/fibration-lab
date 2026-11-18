// Exports of the deck: run one (pptx, pdf, png, outline) through `export_run`, see the history,
// and reveal or open a file.
import { EXPORT_FORMATS, exportDeck, revealExport } from "../../state/deckActions";
import { formatTime } from "../common/format";
import { useDeckContext } from "./context";
import { NoDeck } from "./NoDeck";

export function ExportsPanel() {
  const ctx = useDeckContext();
  if (!ctx) return <NoDeck />;
  const { session, deck, d } = ctx;
  return (
    <div className="panel" data-testid="exports-panel">
      <div className="panel-toolbar">
        <span className="small muted">Export</span>
        {EXPORT_FORMATS.map((f) => (
          <button key={f.format} type="button" className="small" title={f.label} onClick={() => void exportDeck(session, deck, f.format)} data-testid={`export-${f.format}`}>
            {f.format}
          </button>
        ))}
        <span className="grow" />
      </div>
      <div className="panel-body">
        {d.exports.length === 0 ? <div className="empty">No exports yet. Each export takes a checkpoint first, so the file can be traced to the state it was built from.</div> : null}
        <ul className="list">
          {d.exports.map((x) => (
            <li key={x.id} className="row" data-testid="export-item">
              <div className="grow list-item" style={{ cursor: "default" }}>
                <span className="badge accent">{x.format}</span>
                <span className="truncate grow mono small" title={x.path}>
                  {x.path}
                </span>
                <span className="muted small">
                  {formatTime(x.created_at)} · {x.errors} error{x.errors === 1 ? "" : "s"} · {x.warnings} warning{x.warnings === 1 ? "" : "s"}
                </span>
              </div>
              <button type="button" className="ghost small" onClick={() => void revealExport(session, deck, x.id, false)}>
                Reveal
              </button>
              <button type="button" className="ghost small" onClick={() => void revealExport(session, deck, x.id, true)}>
                Open
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
