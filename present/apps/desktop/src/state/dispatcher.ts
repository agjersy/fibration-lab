// Applies ProjectEvents (IPC.md section 3) to the stores. One subscription per
// open project; events for projects that are not active still update state.
import { toast } from "@wb/ui";
import { backend } from "../ipc";
import type { ProjectEvent, ProjectId } from "../ipc";
import { useChat } from "./chat";
import { useChecks } from "./checks";
import { useDeck } from "./deck";
import { useProjects } from "./projects";
import { useRefs } from "./refs";
import { useTasks } from "./tasks";

export function applyEvent(project: ProjectId, e: ProjectEvent): void {
  switch (e.type) {
    case "agent_text":
      useChat.getState().appendText(e.conversation, e.turn, e.delta);
      break;
    case "agent_tool_call":
      useChat.getState().toolCall(e.conversation, e.turn, e.id, e.name, e.arguments);
      break;
    case "agent_tool_result":
      useChat.getState().toolResult(e.conversation, e.id, e.ok, e.summary, e.image);
      break;
    case "proposal":
      useChat.getState().addProposal(project, e.proposal);
      break;
    case "proposal_resolved":
      useChat.getState().resolveProposal(e.proposal_id, e.decision, e.by);
      break;
    case "turn_started":
      useChat.getState().turnStarted(e.conversation, e.turn);
      break;
    case "turn_done":
      useChat.getState().turnDone(e.conversation, e.turn, e.summary);
      break;
    case "turn_error":
      useChat.getState().turnError(e.conversation, e.turn, e.message);
      break;
    case "usage":
      useChat.getState().setUsage(e.conversation, { input_tokens: e.input_tokens, output_tokens: e.output_tokens, context_window: e.context_window, context_used: e.context_used });
      break;
    case "deck_changed": {
      useDeck.getState().applyDeck(project, e.deck, e.removed);
      const info = useProjects.getState().infos[project];
      if (info) useProjects.getState().setInfo({ ...info, deck: e.deck, deck_mode: e.deck.mode, slides: e.deck.sections.reduce((n, s) => n + s.slides.length, 0), head_snapshot: e.head_snapshot });
      if (e.removed.length) useChecks.getState().replace(project, e.removed, []);
      break;
    }
    case "slide_view":
      useDeck.getState().upsertSlide(project, e.view);
      break;
    case "diagnostics":
      useChecks.getState().replace(project, e.slides, e.diagnostics);
      break;
    case "refs_changed":
      useRefs.getState().setRefs(project, e.refs);
      break;
    case "build_output":
      useChat.getState().appendBuildOutput(project, e.chunk);
      break;
    case "export_done":
      useChat.getState().addExport(project, e.export);
      break;
    case "kernel_state":
      useProjects.getState().setKernel(project, e.state, e.message);
      if (e.state === "error" && e.message) toast("error", `Kernel error: ${e.message}`);
      break;
    case "task_progress":
      useTasks.getState().progress(project, e.task_id, e.fraction, e.message);
      break;
    case "task_done":
      useTasks.getState().done(project, e.task_id, e.ok, e.message);
      if (!e.ok && e.message) toast("error", e.message);
      break;
    case "project_changed":
      useProjects.getState().setInfo(e.info);
      break;
  }
}

const subscriptions = new Map<ProjectId, () => void>();

export async function subscribeProject(project: ProjectId): Promise<void> {
  if (subscriptions.has(project)) return;
  subscriptions.set(project, () => undefined);
  const unsubscribe = await backend().subscribe(project, (e) => applyEvent(project, e));
  subscriptions.set(project, unsubscribe);
}

export function unsubscribeProject(project: ProjectId): void {
  const un = subscriptions.get(project);
  if (un) {
    un();
    subscriptions.delete(project);
  }
}
