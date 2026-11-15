// The one path every proposal decision takes, whether it is clicked on the
// chat card or in the review tab. If a review tab is open for the proposal,
// "Accept" sends what that tab currently holds (chunks the user rejected or
// edited there are honoured), and the tab closes only once the backend has
// accepted the decision.
import type { ProposalDecision } from "../../ipc";
import { respondProposal } from "../../state/actions";
import type { ProposalRecord } from "../../state/chat";
import { closePanel } from "../workbench/controller";

/** Open review tabs by proposal id: how to read the reviewed document. */
const reviews = new Map<string, () => string>();

export function registerReview(proposalId: string, getContent: () => string): () => void {
  reviews.set(proposalId, getContent);
  return () => {
    if (reviews.get(proposalId) === getContent) reviews.delete(proposalId);
  };
}

/** The decision "accept" stands for, given the reviewed content (if any) and the proposal as made. */
export function acceptDecision(proposed: string | null, reviewed: string | undefined): ProposalDecision {
  if (reviewed === undefined || proposed === null || reviewed === proposed) return { decision: "accept" };
  return { decision: "accept", content: reviewed };
}

export async function acceptProposal(record: ProposalRecord): Promise<boolean> {
  const { proposal, session } = record;
  const reviewed = reviews.get(proposal.id)?.();
  const ok = await respondProposal(session, proposal.id, acceptDecision(proposal.proposed, reviewed));
  if (ok) closePanel(`proposal:${proposal.id}`);
  return ok;
}

export async function rejectProposal(record: ProposalRecord, reason?: string): Promise<boolean> {
  const { proposal, session } = record;
  const ok = await respondProposal(session, proposal.id, reason ? { decision: "reject", reason } : { decision: "reject" });
  if (ok) closePanel(`proposal:${proposal.id}`);
  return ok;
}
