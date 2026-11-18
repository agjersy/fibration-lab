// The deck checker's findings by slide and rule (stale run citations among them); a row selects
// its slide on the canvas. Same look as the analysis Diagnostics panel.
import { useState } from "react";
import { runDeckChecks } from "../../state/deckActions";
import { useDeckContext } from "./context";
import { showSlide } from "./navigation";
import { NoDeck } from "./NoDeck";

export function ChecksPanel() {
  const ctx = useDeckContext();
  const [checking, setChecking] = useState(false);
  if (!ctx) return <NoDeck />;
  const { session, deck, d } = ctx;
  const rows = [...d.diagnostics].sort((a, b) => (a.slide_index ?? d.slides[a.slide_id ?? ""]?.index ?? -1) - (b.slide_index ?? d.slides[b.slide_id ?? ""]?.index ?? -1) || a.rule.localeCompare(b.rule));
  const errors = rows.filter((x) => x.severity === "error").length;
  const check = async () => {
    setChecking(true);
    await runDeckChecks(session, deck);
    setChecking(false);
  };
  return (
    <div className="panel" data-testid="checks-panel">
      <div className="panel-toolbar">
        <span className="small muted">
          {rows.length} finding{rows.length === 1 ? "" : "s"} · {errors} error{errors === 1 ? "" : "s"}
        </span>
        <span className="grow" />
        <button type="button" className="small" onClick={() => void check()} disabled={checking} data-testid="run-deck-checks">
          {checking ? "Checking…" : "Run checks"}
        </button>
      </div>
      <div className="panel-body">
        {rows.length === 0 ? <div className="empty">No findings. The checker runs after every change; run it here to check the whole deck.</div> : null}
        {rows.length > 0 ? (
          <table className="grid">
            <thead>
              <tr>
                <th>severity</th>
                <th>slide</th>
                <th>rule</th>
                <th>message</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((x, i) => {
                const v = x.slide_id ? d.slides[x.slide_id] : undefined;
                return (
                  <tr key={`${x.slide_id}:${x.element_id}:${x.rule}:${i}`} className="clickable" onClick={() => x.slide_id && showSlide(session, deck, x.slide_id)} data-testid="check-row" data-rule={x.rule}>
                    <td>
                      <span className={`badge ${x.severity === "error" ? "error" : x.severity === "warn" ? "warning" : ""}`}>{x.severity}</span>
                    </td>
                    <td className="mono">{v ? v.index + 1 : x.slide_index !== undefined ? x.slide_index + 1 : "deck"}</td>
                    <td className="mono">{x.rule}</td>
                    <td>
                      {x.message}
                      {x.fix_hint ? <div className="muted">{x.fix_hint}</div> : null}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        ) : null}
      </div>
    </div>
  );
}
