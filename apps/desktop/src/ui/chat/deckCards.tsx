// Deck cards of the one chat: slide proposals as before/after previews (drawn by the slide
// renderer of the deck panels), the storyline as a table, and the results of the deck checker and
// of exports. Decisions go through `proposal_respond` like code diffs.
import { memo, useEffect, useState } from "react";
import type { Deck, SessionId } from "../../ipc";
import { respondProposal } from "../../state/actions";
import { useProposal, useResponding } from "../../state/chat";
import type { ChatItem, ProposalRecord } from "../../state/chat";
import { ensureDeckInfo } from "../../state/deckActions";
import { useDecks, useDeckState } from "../../state/decks";
import type { OutlineRow } from "../../state/decks";
import { useActiveSession, useSessionInfo } from "../../state/sessions";
import { deckFileUrl } from "../../deck/files";
import { slideTitle } from "../../deck/model";
import { acceptProposal, rejectProposal } from "../editor/review";
import { openDeckPanel } from "../workbench/controller";
import { SlidePreview } from "../deck/slide/SlidePreview";

const SLIDE_KINDS = new Set(["slide-create", "slide-update", "slide-delete", "slide-move", "outline", "code-edit", "mode-switch"]);

/** Whether a proposal is a deck change (rendered by `SlideProposalCard`). */
export function isDeckProposal(kind: string, deck: string | undefined): boolean {
  return SLIDE_KINDS.has(kind) && deck !== undefined;
}

function isSlide(x: unknown): x is Deck.Slide {
  return typeof x === "object" && x !== null && "placeholders" in x && "layout" in x;
}

function SlideThumb({ session, deck, slide, label }: { session: SessionId; deck: string; slide: unknown; label: string }) {
  const d = useDeckState(session, deck);
  const info = useSessionInfo(session);
  const template = d?.info?.template;
  useEffect(() => {
    if (!template) void ensureDeckInfo(session, deck);
  }, [session, deck, template]);
  return (
    <figure className="slide-compare-item" data-testid={`slide-${label.toLowerCase()}`}>
      <figcaption className="small muted">{label}</figcaption>
      {!isSlide(slide) ? (
        <div className="slide-compare-empty small muted">{label === "Before" ? "new slide" : "removed"}</div>
      ) : template ? (
        <SlidePreview slide={slide} template={template} deck={d?.deck ?? null} width={176} resolveAsset={(p) => deckFileUrl(session, info?.repo_abs, deck, p)} />
      ) : (
        <div className="slide-compare-empty small muted">{slideTitle(slide) || slide.layout}</div>
      )}
    </figure>
  );
}

function OutlineRows({ rows }: { rows: unknown }) {
  if (!Array.isArray(rows)) return null;
  return (
    <table className="grid">
      <thead>
        <tr>
          <th>#</th>
          <th>action title</th>
          <th>key message</th>
        </tr>
      </thead>
      <tbody>
        {(rows as OutlineRow[]).map((r, i) => (
          <tr key={i}>
            <td className="mono">{i + 1}</td>
            <td>{r.title}</td>
            <td className="muted">{r.message}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function statusLabel(record: ProposalRecord): string {
  const { status, by } = record;
  const base = status === "accept" ? "accepted" : status === "auto" ? "auto" : status === "expired" ? "no longer pending" : "rejected";
  return `${base}${by === "policy" ? " by policy" : ""}`;
}

export const SlideProposalCard = memo(function SlideProposalCard({ proposalId }: { proposalId: string }) {
  const record = useProposal(proposalId);
  const responding = useResponding(proposalId);
  const [editing, setEditing] = useState<string | null>(null);
  if (!record) return null;
  const { proposal, status, session } = record;
  const deck = proposal.deck ?? "";
  const pending = status === "pending";
  const slideKinds = proposal.kind === "slide-create" || proposal.kind === "slide-update" || proposal.kind === "slide-delete";
  const editable = proposal.kind === "slide-create" || proposal.kind === "slide-update" || proposal.kind === "outline";
  const acceptModified = async () => {
    if (editing === null) return;
    try {
      JSON.parse(editing);
    } catch {
      return;
    }
    if (await respondProposal(session, proposal.id, { decision: "accept", content: editing })) setEditing(null);
  };
  let validJson = true;
  if (editing !== null) {
    try {
      JSON.parse(editing);
    } catch {
      validJson = false;
    }
  }
  return (
    <div className="card proposal" data-testid="slide-proposal-card" data-kind={proposal.kind}>
      <div className="card-header" style={{ cursor: "default" }}>
        <span className="badge accent">{proposal.kind}</span>
        <span className="grow truncate">{proposal.summary}</span>
        {!pending ? <span className={`badge ${status === "reject" ? "error" : status === "expired" ? "" : "success"}`}>{statusLabel(record)}</span> : null}
      </div>
      <div className="card-body stack">
        <div className="small muted">
          deck <span className="mono">{deck}</span>
          {proposal.slide_ids?.length ? ` · ${proposal.slide_ids.join(", ")}` : ""}
        </div>
        {slideKinds ? (
          <div className="slide-compare">
            <SlideThumb session={session} deck={deck} slide={proposal.before} label="Before" />
            <span className="subtle" aria-hidden="true">
              →
            </span>
            <SlideThumb session={session} deck={deck} slide={proposal.after} label="After" />
          </div>
        ) : null}
        {proposal.kind === "outline" ? <OutlineRows rows={proposal.after} /> : null}
        {proposal.kind === "code-edit" && proposal.diff ? <pre>{proposal.diff}</pre> : null}
        {proposal.message ? (
          <div className="muted proposal-message" data-testid="proposal-message">
            {proposal.message}
          </div>
        ) : null}
        {proposal.reason !== "review" ? (
          <div className="muted">{proposal.reason === "protected" ? "Protected path" : proposal.reason === "out_of_scope" ? "Out of the conversation's scope" : "Approval is required before this goes ahead"}</div>
        ) : null}
        {editing !== null ? (
          <div className="stack">
            <label className="small muted" htmlFor={`edit-${proposal.id}`}>
              Edit the proposed {proposal.kind === "outline" ? "storyline" : "slide document"} (JSON), then accept your version
            </label>
            <textarea id={`edit-${proposal.id}`} className="mono" rows={10} value={editing} onChange={(e) => setEditing(e.target.value)} data-testid="proposal-edit" />
            {!validJson ? <div className="small" style={{ color: "var(--error)" }}>Not valid JSON.</div> : null}
          </div>
        ) : null}
      </div>
      {pending ? (
        <div className="card-actions">
          <button type="button" className="small" onClick={() => openDeckPanel("canvas")} disabled={!deck}>
            Show deck
          </button>
          <span className="grow" />
          <button type="button" className="danger" onClick={() => void rejectProposal(record)} disabled={responding} data-testid="proposal-reject">
            Reject
          </button>
          {editable ? (
            editing === null ? (
              <button type="button" onClick={() => setEditing(JSON.stringify(proposal.after ?? null, null, 2))} disabled={responding} data-testid="proposal-modify">
                Accept modified…
              </button>
            ) : (
              <button type="button" onClick={() => void acceptModified()} disabled={responding || !validJson} data-testid="proposal-accept-modified">
                Accept my version
              </button>
            )
          ) : null}
          <button type="button" className="primary" onClick={() => void acceptProposal(record)} disabled={responding || editing !== null} data-testid="proposal-accept">
            Accept
          </button>
        </div>
      ) : null}
    </div>
  );
});

/** A deck tool the agent ran whose result deserves more than a tool row: the checker, an export. */
export function deckToolKind(name: string): "check" | "export" | null {
  const n = name.replace(/_/g, ".");
  if (n === "check.run" || n === "deck.check.run") return "check";
  if (n === "export.run") return "export";
  return null;
}

function parseResult(summary: string | undefined): Record<string, unknown> | null {
  if (!summary) return null;
  try {
    const v = JSON.parse(summary) as unknown;
    return typeof v === "object" && v !== null ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

export const DeckToolCard = memo(function DeckToolCard({ item }: { item: Extract<ChatItem, { kind: "tool" }> }) {
  const session = useActiveSession();
  const kind = deckToolKind(item.name);
  const args = (item.arguments ?? {}) as { deck?: string; format?: string };
  const result = parseResult(item.result?.summary);
  const show = () => {
    if (session && args.deck) useDecks.getState().setActive(session, args.deck);
    openDeckPanel(kind === "check" ? "checks" : "exports");
  };
  const errors = typeof result?.errors === "number" ? result.errors : null;
  const warnings = typeof result?.warnings === "number" ? result.warnings : null;
  return (
    <div className="card" data-testid={kind === "check" ? "deck-check-card" : "deck-export-card"}>
      <div className="card-header" style={{ cursor: "default" }}>
        <span className="badge accent">{kind === "check" ? "deck check" : `export ${args.format ?? ""}`}</span>
        <span className="grow truncate">
          {!item.result
            ? "running…"
            : !item.result.ok
              ? item.result.summary
              : kind === "check"
                ? errors !== null
                  ? `${errors} error${errors === 1 ? "" : "s"}, ${warnings ?? 0} warning${warnings === 1 ? "" : "s"}`
                  : item.result.summary
                : typeof result?.path === "string"
                  ? result.path
                  : item.result.summary}
        </span>
        {item.result ? <span className={`badge ${item.result.ok && !errors ? "success" : item.result.ok ? "warning" : "error"}`}>{item.result.ok ? "done" : "failed"}</span> : null}
        <button type="button" className="small" onClick={show}>
          {kind === "check" ? "Show checks" : "Show exports"}
        </button>
      </div>
    </div>
  );
});
