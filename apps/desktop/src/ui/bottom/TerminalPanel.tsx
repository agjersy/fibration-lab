import { Suspense, lazy } from "react";
import { closeTerminal, openTerminal } from "../../state/actions";
import { useActiveSession, useKernel } from "../../state/sessions";
import { useTerminals } from "../../state/terminals";
import type { TerminalTab } from "../../state/terminals";

const XtermView = lazy(() => import("./XtermView"));

const NO_TERMINALS: TerminalTab[] = [];

export function TerminalPanel() {
  const session = useActiveSession();
  const kernel = useKernel(session);
  const terminals = useTerminals((s) => (session ? (s.terminals[session] ?? NO_TERMINALS) : NO_TERMINALS));
  const active = useTerminals((s) => (session ? (s.active[session] ?? null) : null));
  if (!session) return <div className="empty">No session</div>;
  const current = terminals.find((t) => t.id === active) ?? terminals[terminals.length - 1];
  return (
    <div className="panel" data-testid="terminal-panel">
      <div className="terminal-tabs">
        {terminals.map((t) => (
          <button
            key={t.id}
            type="button"
            className={`terminal-tab small${current?.id === t.id ? " active" : ""}`}
            onClick={() => useTerminals.getState().setActive(session, t.id)}
          >
            {t.title}
            {t.exit !== undefined ? " (exited)" : ""}
          </button>
        ))}
        <button type="button" className="ghost small" onClick={() => void openTerminal(session)} aria-label="New terminal" title="New terminal (Ctrl+`)">
          +
        </button>
        <span className="grow" />
        {current ? (
          // The shell's network state, as the backend reported it when the terminal opened.
          <span
            className={`badge ${current.networkIsolated ? "success" : "warning"}`}
            data-testid="terminal-network"
            title={
              current.networkIsolated
                ? `This shell and everything started from it have no network access (${current.networkIsolation}).`
                : `This shell can reach the network: ${current.networkNote ?? "no network isolation is available on this machine"}.`
            }
          >
            {current.networkIsolated ? "no network" : "network not isolated"}
          </span>
        ) : null}
        {current ? (
          <button type="button" className="ghost small" onClick={() => void closeTerminal(session, current.id)} aria-label="Close terminal">
            ×
          </button>
        ) : null}
      </div>
      {current ? (
        // Every terminal keeps its screen mounted; only the active one is visible.
        <div className="terminal-stack">
          <Suspense fallback={<div className="empty">Loading terminal…</div>}>
            {terminals.map((t) => (
              <XtermView key={t.id} terminalId={t.id} exited={t.exit} visible={t.id === current.id} />
            ))}
          </Suspense>
        </div>
      ) : (
        <div className="empty">
          <p>A shell on the session target with the venv active.</p>
          <button type="button" onClick={() => void openTerminal(session)} disabled={kernel.state === "stopped"}>
            Open terminal
          </button>
        </div>
      )}
    </div>
  );
}
