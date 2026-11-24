import { memo, useEffect, useRef } from "react";
import { backend } from "../../ipc";
import type { ConversationId, Mode, SessionId } from "../../ipc";
import { newConversation, selectConversation } from "../../state/actions";
import { useChat, useConversationItems, useProposal } from "../../state/chat";
import type { ChatItem } from "../../state/chat";
import { loadTranscript } from "../../state/loaders";
import { useActiveSession, useSessionInfo, useSessions } from "../../state/sessions";
import { notifyError } from "../../state/ui";
import { MessageText, StreamingMessage } from "./MessageText";
import { ChatInput } from "./ChatInput";
import { ErrorCard, ProposalCard, RefreshFailedCard, RunCard, SummaryCard, ToolCard } from "./cards";
import { DeckToolCard, SlideProposalCard, deckToolKind, isDeckProposal } from "./deckCards";

/** Code diffs and slide proposals side by side in one conversation. */
function ProposalRow({ proposalId }: { proposalId: string }) {
  const record = useProposal(proposalId);
  if (record && isDeckProposal(record.proposal.kind, record.proposal.deck)) return <SlideProposalCard proposalId={proposalId} />;
  return <ProposalCard proposalId={proposalId} />;
}

/** One control loop for both workbenches: the analysis modes and the deck modes. */
export const MODES: { value: Mode; label: string; hint: string }[] = [
  { value: "plan", label: "Plan", hint: "Read-only: the agent proposes a plan" },
  { value: "build", label: "Build", hint: "Code and slide edits are proposed for review" },
  { value: "run", label: "Run only", hint: "Runs existing scripts; no edits" },
  { value: "outline", label: "Outline", hint: "The agent proposes the deck's storyline" },
  { value: "slides", label: "Slides", hint: "The agent proposes slides, shown as before/after cards" },
  { value: "polish", label: "Polish", hint: "The agent tightens existing slides and fixes checker findings" },
];

const EMPTY_TEXT: Record<Mode, string> = {
  plan: "Ask for a plan. Nothing is changed in plan mode.",
  build: "Ask for an analysis. Edits are proposed as diffs for review.",
  run: "Run mode: ask for sweeps and comparisons using existing scripts.",
  outline: "Outline mode: ask for a storyline for a deck.",
  slides: "Slides mode: ask for slides; each is proposed as a before/after card.",
  polish: "Polish mode: ask to tighten slides and fix what the checker reports.",
};

const UserMessage = memo(function UserMessage({ item }: { item: Extract<ChatItem, { kind: "user" }> }) {
  return (
    <div className="msg msg-user" data-testid="user-message">
      {item.text}
      {item.attachments.length ? <div className="small muted">{item.attachments.map((a) => a.path).join(", ")}</div> : null}
    </div>
  );
});

/**
 * Every row is memoised on its item, and items keep their identity unless
 * they change, so adding or updating one item renders one row. Streamed text
 * does not pass through here at all (see StreamingMessage).
 */
const ChatItemRow = memo(function ChatItemRow({ item, session, conversation }: { item: ChatItem; session: SessionId; conversation: ConversationId }) {
  switch (item.kind) {
    case "user":
      return <UserMessage item={item} />;
    case "assistant":
      return item.streaming ? <StreamingMessage conversation={conversation} id={item.id} fallback={item.text} /> : <MessageText text={item.text} />;
    case "tool":
      return deckToolKind(item.name) ? <DeckToolCard item={item} /> : <ToolCard item={item} />;
    case "proposal":
      return <ProposalRow proposalId={item.proposal_id} />;
    case "run":
      return <RunCard session={session} run_id={item.run_id} />;
    case "summary":
      return <SummaryCard summary={item.summary} />;
    case "error":
      return <ErrorCard message={item.message} />;
    case "refresh_failed":
      return <RefreshFailedCard session={session} deck={item.deck} slideId={item.slide_id} message={item.message} />;
  }
});

const MessageList = memo(function MessageList({ session, conversation, emptyText }: { session: SessionId; conversation: ConversationId; emptyText: string }) {
  const items = useConversationItems(conversation);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [items]);

  return (
    <div className="chat-messages" ref={listRef} role="log" aria-live="polite">
      {items.length === 0 ? <div className="empty">{emptyText}</div> : null}
      {items.map((item) => (
        <ChatItemRow key={item.id} item={item} session={session} conversation={conversation} />
      ))}
    </div>
  );
});

export function ChatPanel() {
  const session = useActiveSession();
  const info = useSessionInfo(session);
  const models = useSessions((s) => s.models);
  const conversationId = info?.active_conversation ?? null;
  // Selected field by field: the conversation object changes with every streamed frame.
  const loaded = useChat((s) => (conversationId ? (s.conversations[conversationId]?.loaded ?? false) : false));
  const busy = useChat((s) => (conversationId ? (s.conversations[conversationId]?.busy ?? false) : false));
  const usage = useChat((s) => (conversationId ? (s.conversations[conversationId]?.usage ?? null) : null));
  const summary = info?.conversations.find((c) => c.id === conversationId) ?? null;

  // First display of a conversation: map its stored transcript into chat items.
  useEffect(() => {
    if (!session || !conversationId) return;
    useChat.getState().ensure(conversationId);
    if (!loaded) void loadTranscript(session, conversationId).catch(notifyError);
  }, [session, conversationId, loaded]);

  if (!session || !info || !conversationId) return <div className="empty">No session</div>;

  const setMode = (mode: Mode) =>
    backend()
      .invoke("chat_set_mode", { session, conversation: conversationId, mode })
      .catch(notifyError);
  const setModel = (model: string) =>
    backend()
      .invoke("chat_set_model", { session, conversation: conversationId, model })
      .catch(notifyError);

  const pct = usage && usage.context_window > 0 ? Math.min(100, Math.round((usage.context_used / usage.context_window) * 100)) : 0;

  return (
    <div className="chat" data-testid="chat">
      <div className="panel-toolbar">
        <select
          aria-label="Conversation"
          value={conversationId}
          onChange={(e) => void selectConversation(session, e.target.value)}
          style={{ maxWidth: 160 }}
        >
          {info.conversations.map((c) => (
            <option key={c.id} value={c.id}>
              {c.title}
              {c.busy ? " …" : ""}
            </option>
          ))}
        </select>
        <button type="button" className="small" onClick={() => void newConversation(session)} aria-label="New conversation">
          +
        </button>
        <span className="grow" />
        <select aria-label="Mode" value={summary?.mode ?? "build"} onChange={(e) => void setMode(e.target.value as Mode)} data-testid="mode-select">
          {MODES.map((m) => (
            <option key={m.value} value={m.value} title={m.hint}>
              {m.label}
            </option>
          ))}
        </select>
        <select aria-label="Conversation model" value={summary?.model ?? info.model ?? ""} onChange={(e) => void setModel(e.target.value)} style={{ maxWidth: 150 }}>
          {models.map((m) => (
            <option key={`${m.provider}/${m.id}`} value={`${m.provider}/${m.id}`}>
              {m.name}
            </option>
          ))}
        </select>
      </div>
      <MessageList
        session={session}
        conversation={conversationId}
        emptyText={EMPTY_TEXT[summary?.mode ?? "build"]}
      />
      <div className="row small muted" style={{ padding: "2px 10px" }} title={usage ? `${usage.context_used} of ${usage.context_window} context tokens` : "no usage yet"}>
        <div className="usage" aria-label="Context usage">
          <div style={{ width: `${pct}%` }} />
        </div>
        <span>{usage ? `${pct}% · ${usage.input_tokens + usage.output_tokens} tok` : "—"}</span>
      </div>
      <ChatInput session={session} conversation={conversationId} busy={busy} />
    </div>
  );
}
