// The deck's references: extraction status, which slides cite each, add (native dialog), remove,
// remove the unused ones, and search (hits open the source passage).
import { useState } from "react";
import { backend } from "../../ipc";
import type { Deck } from "../../ipc";
import { addReferences, removeReference, removeUnusedReferences } from "../../state/deckActions";
import { notifyError } from "../../state/ui";
import { formatBytes } from "../common/format";
import { useDeckContext } from "./context";
import { openCite } from "./navigation";
import { NoDeck } from "./NoDeck";

export function ReferencesPanel() {
  const ctx = useDeckContext();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Deck.SearchHit[] | null>(null);
  if (!ctx) return <NoDeck />;
  const { session, deck, d } = ctx;
  const search = async () => {
    if (!query.trim()) {
      setHits(null);
      return;
    }
    try {
      setHits(await backend().invoke("refs_search", { session, deck, query: query.trim() }));
    } catch (e) {
      notifyError(e);
    }
  };
  return (
    <div className="panel" data-testid="references-panel">
      <div className="panel-toolbar">
        <input
          type="search"
          className="grow"
          aria-label="Search references"
          placeholder="Search references…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && void search()}
        />
        <button type="button" className="small" onClick={() => void addReferences(session, deck)} data-testid="add-references">
          Add…
        </button>
        <button type="button" className="small" onClick={() => void removeUnusedReferences(session, deck)} disabled={d.refs.length === 0}>
          Remove unused
        </button>
      </div>
      <div className="panel-body">
        {hits ? (
          <>
            <div className="row small muted" style={{ padding: "4px 8px" }}>
              <span className="grow">
                {hits.length} hit{hits.length === 1 ? "" : "s"} for “{query}”
              </span>
              <button type="button" className="ghost small" onClick={() => setHits(null)}>
                Clear
              </button>
            </div>
            <ul className="list">
              {hits.map((h) => (
                <li key={h.cite}>
                  <button type="button" className="list-item" onClick={() => openCite(session, deck, h.cite)} data-testid="ref-hit">
                    <div className="grow" style={{ padding: "3px 0" }}>
                      <div className="truncate">{h.title}</div>
                      <div className="small muted truncate">{h.snippet}</div>
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          </>
        ) : null}
        {d.refs.length === 0 ? <div className="empty">No references. Add PDFs, spreadsheets, documents or images for slides to cite.</div> : null}
        <ul className="list">
          {d.refs.map((r) => (
            <li key={r.id} className="row" data-testid="ref-item">
              <div className="grow list-item" style={{ cursor: "default" }}>
                <div className="grow" style={{ padding: "3px 0" }}>
                  <div className="row">
                    <span className="badge">{r.kind}</span>
                    <span className="truncate grow" title={r.title}>
                      {r.title}
                    </span>
                    <span className={`badge ${r.status === "ready" ? "success" : r.status === "error" ? "error" : "warning"}`} title={r.error ?? undefined}>
                      {r.status}
                    </span>
                  </div>
                  <div className="small muted truncate">
                    {formatBytes(r.size_bytes)}
                    {r.pages !== null ? ` · ${r.pages} page${r.pages === 1 ? "" : "s"}` : ""} · {r.cited_by.length ? `cited by ${r.cited_by.length} slide${r.cited_by.length === 1 ? "" : "s"}` : "not cited"}
                    {r.error ? ` · ${r.error}` : ""}
                  </div>
                </div>
              </div>
              <button type="button" className="ghost small" disabled={r.cited_by.length > 0} title={r.cited_by.length ? "Cited by slides; remove the citations first" : "Remove this reference"} onClick={() => void removeReference(session, deck, r.id)} aria-label={`Remove ${r.title}`}>
                ×
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
