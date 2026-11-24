import { memo, useMemo, useRef, useState } from "react";
import type { FigureView, RunId, SessionId, TurnSummary } from "../../ipc";
import { useProposal, useResponding } from "../../state/chat";
import { useFigureSrc } from "../../state/figureCache";
import { groupFigures } from "../../state/figures";
import type { ChatItem } from "../../state/chat";
import { useRuns } from "../../state/runs";
import { useVisibleOnce } from "../common/useVisibleOnce";
import { compactJson, formatDuration, shortArgs } from "../common/format";
import { refreshFromAnalysis } from "../../state/deckActions";
import { acceptProposal, rejectProposal } from "../editor/review";
import { openFile, openProposal, showPanel } from "../workbench/controller";

// The cards are memoised: a conversation re-renders one card when its item or record changes, not all of them.

export const ToolCard = memo(function ToolCard({ item }: { item: Extract<ChatItem, { kind: "tool" }> }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="card" data-testid="tool-card">
      <button type="button" className="card-header" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="subtle">{open ? "▾" : "▸"}</span>
        <span className="mono">{item.name}</span>
        <span className="muted truncate grow">{compactJson(item.arguments, 60)}</span>
        {item.result ? <span className={`badge ${item.result.ok ? "success" : "error"}`}>{item.result.ok ? "ok" : "failed"}</span> : <span className="badge">running</span>}
      </button>
      {open ? (
        <div className="card-body stack">
          <div>
            <div className="subtle">arguments</div>
            <pre>{JSON.stringify(item.arguments, null, 2)}</pre>
          </div>
          {item.result ? (
            <div>
              <div className="subtle">result</div>
              <pre>{item.result.summary}</pre>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
});

export const ProposalCard = memo(function ProposalCard({ proposalId }: { proposalId: string }) {
  const record = useProposal(proposalId);
  const responding = useResponding(proposalId);
  if (!record) return null;
  const { proposal, status, by } = record;
  const pending = status === "pending";
  const reviewable = (proposal.kind === "edit" || proposal.kind === "create") && proposal.proposed !== null;
  return (
    <div className={`card proposal`} data-testid="proposal-card">
      <div className="card-header" style={{ cursor: "default" }}>
        <span className="badge accent">{proposal.kind}</span>
        <span className="grow truncate">{proposal.summary}</span>
        {!pending ? (
          <span className={`badge ${status === "reject" ? "error" : status === "expired" ? "" : "success"}`}>
            {status === "accept" ? "accepted" : status === "auto" ? "auto" : status === "expired" ? "no longer pending" : "rejected"}
            {by === "policy" ? " by policy" : ""}
          </span>
        ) : null}
      </div>
      <div className="card-body">
        {proposal.path ? <div className="mono">{proposal.path}</div> : null}
        {proposal.command ? <pre>$ {proposal.command}</pre> : null}
        {proposal.packages ? <div className="mono">{proposal.packages.join(", ")}</div> : null}
        {proposal.message ? <pre>{proposal.message}</pre> : null}
        {proposal.reason !== "review" ? (
          <div className="muted">
            {proposal.reason === "protected" ? "Protected path" : proposal.reason === "out_of_scope" ? "Out of the conversation's scope" : "Approval is required before this goes ahead"}
          </div>
        ) : null}
        {!reviewable && proposal.diff ? <pre>{proposal.diff}</pre> : null}
      </div>
      {pending ? (
        <div className="card-actions">
          {reviewable ? (
            <button type="button" onClick={() => openProposal(proposal.id, proposal.path ?? proposal.summary)} data-testid="review-proposal">
              Review
            </button>
          ) : null}
          <span className="grow" />
          <button type="button" className="danger" onClick={() => void rejectProposal(record)} disabled={responding} data-testid="proposal-reject">
            Reject
          </button>
          {/* With a review tab open, this accepts what that tab shows (see ui/editor/review.ts). */}
          <button type="button" className="primary" onClick={() => void acceptProposal(record)} disabled={responding} data-testid="proposal-accept">
            Accept
          </button>
        </div>
      ) : null}
    </div>
  );
});

/** Loads its image only once it scrolls into view: a long conversation holds many run cards. */
function FigureThumb({ session, figure, name, run_id }: { session: SessionId; figure: FigureView; name: string; run_id: RunId }) {
  const ref = useRef<HTMLButtonElement>(null);
  const visible = useVisibleOnce(ref);
  const data = useFigureSrc(session, figure, visible);
  const open = () => {
    useRuns.getState().setActiveFigure(session, { name, run_id });
    showPanel("figures");
  };
  return (
    <button ref={ref} type="button" className="thumb" onClick={open} title={`Open ${name} in Figures`} aria-label={`Open figure ${name}`} data-testid="figure-thumb">
      {data && figure.format !== "pdf" ? <img src={data} alt={name} loading="lazy" decoding="async" /> : <span className="subtle small">{name}</span>}
    </button>
  );
}

export const RunCard = memo(function RunCard({ session, run_id }: { session: SessionId; run_id: RunId }) {
  const run = useRuns((s) => s.runs[session]?.find((r) => r.meta.run_id === run_id));
  const files = useRuns((s) => s.figures[session]?.find((g) => g.run.meta.run_id === run_id)?.figures);
  // One thumbnail per figure, whatever formats it was saved in.
  const figures = useMemo(() => groupFigures(files ?? []), [files]);
  if (!run) return null;
  const status = run.meta.status === "done" && run.meta.exit_code !== 0 ? "exit" : run.meta.status;
  const open = () => {
    useRuns.getState().selectRun(session, run_id);
    showPanel("runoutput");
  };
  return (
    <div className="card" data-testid="run-card">
      <button type="button" className="card-header" onClick={open}>
        <span className={`dot ${run.meta.status === "running" ? "starting" : status === "done" ? "ready" : "error"}`} />
        <span className="mono">{run.meta.script}</span>
        <span className="muted truncate grow">{shortArgs(run.meta.args)}</span>
        <span className={`status-${status}`}>{run.meta.status === "done" ? `exit ${run.meta.exit_code}` : run.meta.status}</span>
        <span className="subtle">{formatDuration(run.meta.started_at, run.meta.finished_at)}</span>
      </button>
      {figures.length > 0 ? (
        <div className="card-body">
          <div className="thumbs">
            {figures.map((f) => (
              <FigureThumb key={f.name} session={session} figure={f.primary} name={f.name} run_id={run_id} />
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
});

export const SummaryCard = memo(function SummaryCard({ summary }: { summary: TurnSummary }) {
  return (
    <div className="card" data-testid="summary-card">
      <div className="card-header" style={{ cursor: "default" }}>
        <span className="badge success">turn done</span>
        <span className="grow">{summary.text}</span>
      </div>
      {summary.files_changed.length || summary.runs.length || summary.figures.length ? (
        <div className="card-body stack">
          {summary.files_changed.length ? (
            <div className="row" style={{ flexWrap: "wrap" }}>
              <span className="subtle">files</span>
              {summary.files_changed.map((p) => (
                <button key={p} type="button" className="ghost small mono" onClick={() => void openFile(p)}>
                  {p}
                </button>
              ))}
            </div>
          ) : null}
          {summary.runs.length ? (
            <div className="row">
              <span className="subtle">runs</span>
              <span className="mono">{summary.runs.length}</span>
            </div>
          ) : null}
          {summary.figures.length ? (
            <div className="row">
              <span className="subtle">figures</span>
              <span className="mono">{summary.figures.map((f) => f.split("/").pop()).join(", ")}</span>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
});

/** An accepted refresh that could not be written because the slide changed meanwhile. */
export const RefreshFailedCard = memo(function RefreshFailedCard({ session, deck, slideId, message }: { session: SessionId; deck: string; slideId: string; message: string }) {
  const [busy, setBusy] = useState(false);
  return (
    <div className="card error" data-testid="refresh-failed-card">
      <div className="card-header" style={{ cursor: "default" }}>
        <span className="badge error">refresh not applied</span>
        <span className="grow">{message}</span>
        <button
          type="button"
          className="small"
          disabled={busy}
          data-testid="refresh-again"
          onClick={() => {
            setBusy(true);
            void refreshFromAnalysis(session, deck, slideId).finally(() => setBusy(false));
          }}
        >
          Refresh again
        </button>
      </div>
    </div>
  );
});

export const ErrorCard = memo(function ErrorCard({ message }: { message: string }) {
  return (
    <div className="card error" data-testid="error-card">
      <div className="card-header" style={{ cursor: "default" }}>
        <span className="badge error">error</span>
        <span className="grow">{message}</span>
      </div>
    </div>
  );
});
