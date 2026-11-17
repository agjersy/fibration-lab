// Bottom dock: Sources, Checks, Notes, Exports. These are placeholders for work packages D7 and D8;
// each already shows the state the stores hold (the passage of a clicked citation, the diagnostics,
// the notes of the selected slide, finished exports) without the editing and navigation of the final panels.
import { slideTitle } from "../../deck/model";
import { useChat } from "../../state/chat";
import { useProjectDiagnostics } from "../../state/checks";
import { selectedView, useDeck, useProjectDeck } from "../../state/deck";
import { useActiveProject } from "../../state/projects";
import { useRefs } from "../../state/refs";

export function SourcesPanel() {
  const project = useActiveProject();
  const source = useRefs((s) => (project ? s.source[project] : null)) ?? null;
  if (!source) {
    return (
      <div className="panel" data-testid="sources-panel">
        <div className="empty">Click a citation marker on the slide to see its source passage here.</div>
      </div>
    );
  }
  const p = source.passage;
  return (
    <div className="panel" data-testid="sources-panel">
      <div className="panel-toolbar">
        <b className="truncate">{p?.ref_title ?? "Source"}</b>
        <span className="muted small mono truncate">{source.cite}</span>
      </div>
      <div className="panel-body padded">
        {source.error ? <div style={{ color: "var(--error)" }}>{source.error}</div> : null}
        {!p && !source.error ? <div className="muted">Loading…</div> : null}
        {p?.text != null ? (
          <div className="source-text" data-testid="source-text">
            {p.highlight ? (
              <>
                {p.text.slice(0, p.highlight[0])}
                <mark>{p.text.slice(p.highlight[0], p.highlight[1])}</mark>
                {p.text.slice(p.highlight[1])}
              </>
            ) : (
              p.text
            )}
          </div>
        ) : null}
        {p?.table ? (
          <table className="grid" data-testid="source-table">
            <thead>
              <tr>
                {p.table.columns.map((c) => (
                  <th key={c}>{c}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {p.table.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((v, c) => {
                    const h = p.highlight_cells;
                    const hl = h && r >= h[0] && r <= h[2] && c >= h[1] && c <= h[3];
                    return (
                      <td key={c} className={hl ? "hl" : undefined}>
                        {v ?? ""}
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
      </div>
    </div>
  );
}

export function ChecksPanel() {
  const project = useActiveProject();
  const diagnostics = useProjectDiagnostics(project);
  const deck = useProjectDeck(project);
  return (
    <div className="panel" data-testid="checks-panel">
      {diagnostics.length === 0 ? <div className="empty">No findings.</div> : null}
      <div className="panel-body">
        <div className="list">
          {diagnostics.map((d, i) => (
            <div key={i} className="list-item" onClick={() => project && d.slide_id && useDeck.getState().select(project, d.slide_id)} title={d.fix_hint ?? ""}>
              <span className={`badge ${d.severity === "error" ? "error" : d.severity === "warn" ? "warning" : ""}`}>{d.severity}</span>
              <span className="muted small">{d.slide_id && deck?.slides[d.slide_id] ? `Slide ${deck.slides[d.slide_id]!.index + 1}` : "Deck"}</span>
              <span className="mono small">{d.rule}</span>
              <span className="truncate grow">{d.message}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

export function NotesPanel() {
  const project = useActiveProject();
  const view = selectedView(useProjectDeck(project));
  const notes = view?.slide.notes;
  return (
    <div className="panel" data-testid="notes-panel">
      {view ? (
        <div className="panel-body padded notes-text">
          <div className="muted small">Speaker notes · {slideTitle(view.slide) || `Slide ${view.index + 1}`}</div>
          {notes?.paragraphs.length ? notes.paragraphs.map((p, i) => <p key={i}>{p.runs.map((r) => r.t).join("")}</p>) : <p className="muted">No notes on this slide.</p>}
        </div>
      ) : (
        <div className="empty">Select a slide to see its notes.</div>
      )}
    </div>
  );
}

export function ExportsPanel() {
  const project = useActiveProject();
  const exports = useChat((s) => (project ? s.exports[project] : undefined)) ?? [];
  return (
    <div className="panel" data-testid="exports-panel">
      {exports.length === 0 ? <div className="empty">No exports yet. Presets, running an export and opening the file arrive with the exports panel.</div> : null}
      <div className="panel-body">
        <div className="list">
          {exports.map((e) => (
            <div key={e.id} className="list-item">
              <span className="badge accent">{e.format}</span>
              <span className="truncate grow mono small">{e.path}</span>
              <span className="muted small">
                {e.errors} errors · {e.warnings} warnings
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
