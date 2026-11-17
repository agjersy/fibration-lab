// Placeholder of the agent chat (the chat itself is work package D6). The mode switch is live: it
// issues `chat_set_mode` for the active conversation.
import { setMode } from "../../state/actions";
import { useChat } from "../../state/chat";
import { useActiveProject, useProjectInfo } from "../../state/projects";
import type { Mode } from "../../ipc";

const MODES: { id: Mode; label: string; hint: string }[] = [
  { id: "outline", label: "Outline", hint: "Plan the storyline" },
  { id: "build", label: "Build", hint: "Draft and change slides" },
  { id: "polish", label: "Polish", hint: "Review and tighten" },
];

export function ChatPlaceholder() {
  const project = useActiveProject();
  const info = useProjectInfo(project);
  const conv = info?.conversations.find((c) => c.id === info.active_conversation);
  const items = useChat((s) => (conv ? (s.conversations[conv.id]?.items.length ?? 0) : 0));
  return (
    <div className="panel" data-testid="chat-panel">
      <div className="panel-toolbar">
        <span className="muted small">Mode</span>
        <div className="segmented" role="group" aria-label="Chat mode" data-testid="mode-switch">
          {MODES.map((m) => (
            <button key={m.id} type="button" className="small" aria-pressed={conv?.mode === m.id} title={m.hint} disabled={!project || !conv} onClick={() => project && void setMode(project, m.id)}>
              {m.label}
            </button>
          ))}
        </div>
      </div>
      <div className="empty">
        <b>Agent chat</b>
        <div>The conversation, proposal cards and attachments arrive with the chat work package.</div>
        {items > 0 ? <div className="small">{items} event(s) received in this conversation.</div> : null}
      </div>
    </div>
  );
}
