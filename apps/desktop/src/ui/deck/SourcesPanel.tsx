// The source passage of a clicked reference citation: the passage text with the cited span
// marked, the cited cells of a table, or the page image (through the asset protocol).
import { useDeckContext } from "./context";
import { NoDeck } from "./NoDeck";

export function SourcesPanel() {
  const ctx = useDeckContext();
  if (!ctx) return <NoDeck />;
  const source = ctx.d.source;
  if (!source) {
    return (
      <div className="panel" data-testid="sources-panel">
        <div className="empty">Click a citation on a slide to see its source passage here.</div>
      </div>
    );
  }
  const p = source.passage;
  const page = p?.page_png ? ctx.resolve(p.page_png) : null;
  return (
    <div className="panel" data-testid="sources-panel">
      <div className="panel-toolbar">
        <b className="truncate">{p?.ref_title ?? "Source"}</b>
        <span className="grow" />
        <span className="muted small mono truncate" title={source.cite}>
          {source.cite}
        </span>
      </div>
      <div className="panel-body padded">
        {source.error ? (
          <div role="alert" style={{ color: "var(--error)" }}>
            {source.error}
          </div>
        ) : null}
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
                        {v === null ? "" : String(v)}
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
        {page ? <img className="source-page" src={page} alt={`Page of ${p?.ref_title ?? "the source"}`} data-testid="source-page" /> : null}
      </div>
    </div>
  );
}
