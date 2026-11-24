// Ported from Fibration Lab `crates/core/src/agent/mod.rs` (`maybe_compact`,
// `context_tokens`, `message_tokens`) at commit f229e4d.
// Adapted: works on a message list with a caller-supplied summarizer instead of the agent's
// own model; can use the token count the endpoint reported; never leaves tool results
// without their call at the start of the kept tail; does not re-summarize a summary.
//! Compaction: when the context exceeds a fraction of the model's window, older turns are
//! replaced by a summary. Pinned facts (see `pinned`) live in the system prompt and are not
//! touched, so they survive every compaction.

use crate::event::Event;
use crate::util::{estimate_tokens, truncate_bytes};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::future::Future;
use std::sync::Arc;
use wb_providers::{ChatRequest, ContentPart, Message, ModelClient, Role, StopReason};

/// First line of the user message that carries a summary.
pub const SUMMARY_PREFIX: &str = "[Summary of earlier conversation]\n";
/// Assistant reply that follows a summary when the kept tail starts with a user message.
pub const SUMMARY_ACK: &str = "Understood; continuing from that summary.";
/// System prompt of `ModelSummarizer`.
pub const SUMMARIZER_SYSTEM: &str = "You compact conversation history. Produce a dense summary that preserves every decision, file name, function name, identifier, parameter value, run id, error, and open question, and the user's most recent request. Use short bullet points.";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionConfig {
    /// The model's context window in tokens.
    pub context_window: u32,
    /// Compact when the context exceeds this fraction of the window.
    pub fraction: f32,
    /// Histories shorter than this are never compacted.
    pub min_messages: usize,
}

impl CompactionConfig {
    /// Analyst's defaults: 0.7 of the window, at least 4 messages.
    pub fn new(context_window: u32) -> Self {
        CompactionConfig {
            context_window,
            fraction: 0.7,
            min_messages: 4,
        }
    }

    pub fn with_fraction(mut self, fraction: f32) -> Self {
        self.fraction = fraction;
        self
    }

    /// Token budget above which compaction starts.
    pub fn budget(&self) -> usize {
        (self.context_window as f32 * self.fraction) as usize
    }
}

/// How to count the tokens of the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextEstimate {
    /// Estimated tokens outside the history: system prompt and tool definitions.
    pub overhead_tokens: usize,
    /// `(input_tokens, history_len)`: the input token count the endpoint reported for a
    /// request that carried the first `history_len` messages of the current history. When
    /// set, only the messages added since are estimated.
    pub known: Option<(u64, usize)>,
}

impl ContextEstimate {
    /// Heuristic only (4 characters per token), with the system prompt as overhead.
    pub fn heuristic(system_prompt: &str) -> Self {
        ContextEstimate {
            overhead_tokens: estimate_tokens(system_prompt),
            known: None,
        }
    }

    pub fn with_overhead(mut self, tokens: usize) -> Self {
        self.overhead_tokens = tokens;
        self
    }

    /// Use the usage of the last model call, made when the history had `history_len`
    /// messages. Forget it (`known = None`) after a compaction.
    pub fn with_usage(mut self, input_tokens: u64, history_len: usize) -> Self {
        self.known = Some((input_tokens, history_len));
        self
    }

    /// Estimated input tokens of a request carrying `history`.
    pub fn tokens(&self, history: &[Message]) -> usize {
        match self.known {
            Some((input, len)) if len <= history.len() => {
                input as usize + history_tokens(&history[len..])
            }
            _ => self.overhead_tokens + history_tokens(history),
        }
    }
}

/// Estimated tokens of one message.
pub fn message_tokens(m: &Message) -> usize {
    m.content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => estimate_tokens(text),
            ContentPart::Image { data, .. } => data.len() / 4 / 3,
            ContentPart::Document { data, .. } => data.len() / 4 / 3,
            ContentPart::ToolCall { arguments, .. } => estimate_tokens(&arguments.to_string()) + 10,
            ContentPart::ToolResult { content, .. } => estimate_tokens(content) + 10,
        })
        .sum::<usize>()
        + 4
}

pub fn history_tokens(history: &[Message]) -> usize {
    history.iter().map(message_tokens).sum()
}

/// Summarizes a rendered conversation segment. Implemented for async closures
/// `Fn(String) -> impl Future<Output = anyhow::Result<String>>` and by `ModelSummarizer`.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, segment: String) -> Result<String>;
}

#[async_trait]
impl<F, Fut> Summarizer for F
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = Result<String>> + Send,
{
    async fn summarize(&self, segment: String) -> Result<String> {
        self(segment).await
    }
}

/// Summarizes with a model (Analyst's compaction request).
pub struct ModelSummarizer {
    pub client: Arc<dyn ModelClient>,
    pub max_tokens: u32,
    /// Copied into the request's `audit_tag`.
    pub audit_tag: Option<String>,
}

impl ModelSummarizer {
    pub fn new(client: Arc<dyn ModelClient>) -> Self {
        ModelSummarizer {
            client,
            max_tokens: 1500,
            audit_tag: None,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

#[async_trait]
impl Summarizer for ModelSummarizer {
    async fn summarize(&self, segment: String) -> Result<String> {
        let req = ChatRequest {
            model: self.client.model_id().to_string(),
            system: Some(SUMMARIZER_SYSTEM.into()),
            messages: vec![Message::user_text(format!(
                "Summarize this conversation segment:\n\n{segment}"
            ))],
            tools: vec![],
            max_tokens: Some(self.max_tokens),
            audit_tag: self.audit_tag.clone(),
        };
        let c = wb_providers::collect(self.client.stream(req)).await;
        // Only a completed response is a summary: an error, or a stream that never
        // reported a stop, is a failure.
        if !matches!(c.stop, Some(StopReason::End) | Some(StopReason::MaxTokens)) {
            return Err(anyhow!(
                "model error: {}",
                c.stop_message.unwrap_or_else(|| "no response".into())
            ));
        }
        Ok(c.text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// The context is within the budget.
    NotNeeded,
    /// Over the budget, but there is nothing older to summarize.
    NothingToCompact,
    /// The summarizer failed or returned nothing; the history is unchanged.
    Failed(String),
    Compacted {
        summary: String,
        /// Messages replaced by the summary.
        summarized_messages: usize,
        /// Trailing messages kept verbatim.
        kept_messages: usize,
        tokens_before: usize,
        tokens_after: usize,
    },
}

impl CompactionOutcome {
    pub fn is_compacted(&self) -> bool {
        matches!(self, CompactionOutcome::Compacted { .. })
    }

    /// The transcript event to append so that `replay` rebuilds the compacted history.
    pub fn event(&self) -> Option<Event> {
        match self {
            CompactionOutcome::Compacted {
                summary,
                kept_messages,
                ..
            } => Some(Event::compaction(summary.clone(), *kept_messages)),
            _ => None,
        }
    }
}

fn is_genuine_user(m: &Message) -> bool {
    m.role == Role::User
        && m.content.iter().all(|p| {
            !matches!(
                p,
                ContentPart::ToolResult { .. } | ContentPart::ToolCall { .. }
            )
        })
        && !is_summary(m)
}

fn has_tool_results(m: &Message) -> bool {
    m.content
        .iter()
        .any(|p| matches!(p, ContentPart::ToolResult { .. }))
}

fn is_summary(m: &Message) -> bool {
    m.role == Role::User
        && matches!(m.content.first(), Some(ContentPart::Text { text }) if text.starts_with(SUMMARY_PREFIX))
}

/// Move a tail start backwards until the tail does not begin with tool results (their
/// calls are in the assistant message before them).
pub(crate) fn tail_start(history: &[Message], mut start: usize) -> usize {
    start = start.min(history.len());
    while start > 0 && start < history.len() && has_tool_results(&history[start]) {
        start -= 1;
    }
    start
}

/// The messages that stand for the summarized part.
pub(crate) fn summary_messages(summary: &str, with_ack: bool) -> Vec<Message> {
    let mut v = vec![Message::user_text(format!(
        "{SUMMARY_PREFIX}{}",
        summary.trim()
    ))];
    if with_ack {
        v.push(Message::assistant_text(SUMMARY_ACK));
    }
    v
}

/// Index where the kept tail starts, or `None` when nothing older can be summarized.
fn choose_split(history: &[Message], est: &ContextEstimate, budget: usize) -> Option<usize> {
    let len = history.len();
    // Everything before the split must hold something that is not already a summary.
    let first_real = if history.first().is_some_and(is_summary) {
        if matches!(history.get(1), Some(m) if m.role == Role::Assistant && m.text() == SUMMARY_ACK)
        {
            2
        } else {
            1
        }
    } else {
        0
    };
    let useful = |split: usize| split >= 2 && split > first_real && split < len;
    // The latest genuine user message that is not the last message: keep whole turns.
    let by_turn = (2..len.saturating_sub(1))
        .rev()
        .find(|&i| is_genuine_user(&history[i]))
        .filter(|&i| useful(i));
    // Inside the current turn: keep the last model call and its results.
    let in_turn = Some(tail_start(history, len.saturating_sub(1))).filter(|&i| useful(i));
    match (by_turn, in_turn) {
        (Some(t), Some(i)) if i > t => {
            let tail_tokens = est.overhead_tokens + history_tokens(&history[t..]);
            Some(if tail_tokens > budget { i } else { t })
        }
        (Some(t), _) => Some(t),
        (None, i) => i,
    }
}

/// Render messages as plain text for the summarizer. Long parts are cut.
pub fn render_segment(messages: &[Message]) -> String {
    let mut transcript = String::new();
    for m in messages {
        let role = match m.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        for p in &m.content {
            match p {
                ContentPart::Text { text } => {
                    transcript.push_str(&format!("{role}: {}\n", truncate_bytes(text, 4000)))
                }
                ContentPart::ToolCall {
                    name, arguments, ..
                } => transcript.push_str(&format!(
                    "{role} CALL {name} {}\n",
                    truncate_bytes(&arguments.to_string(), 500)
                )),
                ContentPart::ToolResult { content, .. } => {
                    transcript.push_str(&format!("RESULT {}\n", truncate_bytes(content, 1500)))
                }
                ContentPart::Image { .. } => transcript.push_str("[image]\n"),
                ContentPart::Document { title, .. } => transcript.push_str(&format!(
                    "[document{}]\n",
                    title.as_ref().map(|t| format!(": {t}")).unwrap_or_default()
                )),
            }
        }
    }
    transcript
}

/// Whether the next request would exceed the compaction budget.
pub fn needs_compaction(
    history: &[Message],
    cfg: &CompactionConfig,
    est: &ContextEstimate,
) -> bool {
    est.tokens(history) > cfg.budget() && history.len() >= cfg.min_messages
}

/// Summarize older turns when the context exceeds the configured fraction of the window.
///
/// The kept tail starts at the latest user message that is not the last message, so whole
/// turns are kept; when that tail alone is over the budget (one long turn), only the last
/// model call and its results are kept. The summarized part becomes a user message
/// `[Summary of earlier conversation]...`, followed by a short assistant acknowledgement
/// when the tail starts with a user message. On failure the history is left as it is.
pub async fn maybe_compact(
    history: &mut Vec<Message>,
    cfg: &CompactionConfig,
    est: &ContextEstimate,
    summarizer: &dyn Summarizer,
) -> CompactionOutcome {
    let tokens_before = est.tokens(history);
    let budget = cfg.budget();
    if tokens_before <= budget {
        return CompactionOutcome::NotNeeded;
    }
    if history.len() < cfg.min_messages {
        return CompactionOutcome::NothingToCompact;
    }
    let Some(split) = choose_split(history, est, budget) else {
        return CompactionOutcome::NothingToCompact;
    };
    let segment = render_segment(&history[..split]);
    let summary = match summarizer.summarize(segment).await {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        Ok(_) => {
            tracing::warn!("compaction summary was empty; keeping history");
            return CompactionOutcome::Failed("the summarizer returned no text".into());
        }
        Err(e) => {
            tracing::warn!("compaction summary failed; keeping history: {e:#}");
            return CompactionOutcome::Failed(format!("{e:#}"));
        }
    };
    let tail = history.split_off(split);
    let with_ack = matches!(tail.first(), Some(m) if m.role == Role::User);
    let kept_messages = tail.len();
    *history = summary_messages(&summary, with_ack);
    history.extend(tail);
    CompactionOutcome::Compacted {
        summary,
        summarized_messages: split,
        kept_messages,
        tokens_before,
        tokens_after: est.overhead_tokens + history_tokens(history),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::{to_messages, ReplayOptions};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use wb_providers::{ModelEvent, ScriptedModelClient};

    fn tool_round(id: &str, result_len: usize) -> [Message; 2] {
        [
            Message {
                role: Role::Assistant,
                content: vec![ContentPart::ToolCall {
                    id: id.into(),
                    name: "refs_read".into(),
                    arguments: json!({"cite": id}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentPart::ToolResult {
                    tool_call_id: id.into(),
                    content: "r".repeat(result_len),
                    is_error: false,
                }],
            },
        ]
    }

    /// Two finished turns and the user message of a third.
    fn three_turns(result_len: usize) -> Vec<Message> {
        let mut h = vec![Message::user_text("first task: revenue slide")];
        h.extend(tool_round("t1", result_len));
        h.push(Message::assistant_text("revenue slide done"));
        h.push(Message::user_text("second task: cost slide"));
        h.extend(tool_round("t2", result_len));
        h.push(Message::assistant_text("cost slide done"));
        h.push(Message::user_text("third task: summary slide"));
        h
    }

    struct Fake {
        calls: AtomicUsize,
        segments: Mutex<Vec<String>>,
    }

    fn fake() -> Fake {
        Fake {
            calls: AtomicUsize::new(0),
            segments: Mutex::new(vec![]),
        }
    }

    #[async_trait]
    impl Summarizer for Fake {
        async fn summarize(&self, segment: String) -> Result<String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.segments.lock().unwrap().push(segment);
            Ok(format!("  - summary #{n}\n"))
        }
    }

    #[tokio::test]
    async fn under_budget_or_too_short_is_left_alone() {
        let f = fake();
        let mut h = three_turns(100);
        let before = h.clone();
        let cfg = CompactionConfig::new(100_000);
        let est = ContextEstimate::heuristic("system prompt");
        assert!(!needs_compaction(&h, &cfg, &est));
        assert_eq!(
            maybe_compact(&mut h, &cfg, &est, &f).await,
            CompactionOutcome::NotNeeded
        );
        // Over budget but only three messages.
        let mut short = vec![
            Message::user_text("x".repeat(10_000)),
            Message::assistant_text("y"),
            Message::user_text("z"),
        ];
        let tiny = CompactionConfig::new(1000);
        assert!(!needs_compaction(&short, &tiny, &est));
        assert_eq!(
            maybe_compact(&mut short, &tiny, &est, &f).await,
            CompactionOutcome::NothingToCompact
        );
        assert_eq!(h, before);
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn summarizes_older_turns_and_keeps_the_latest_user_message() {
        let f = fake();
        let mut h = three_turns(4000);
        let cfg = CompactionConfig::new(2000).with_fraction(0.5);
        let est = ContextEstimate::heuristic("sys");
        assert!(needs_compaction(&h, &cfg, &est));
        let out = maybe_compact(&mut h, &cfg, &est, &f).await;
        let CompactionOutcome::Compacted {
            summary,
            summarized_messages,
            kept_messages,
            tokens_before,
            tokens_after,
        } = &out
        else {
            panic!("not compacted: {out:?}");
        };
        assert_eq!(summary, "- summary #1");
        assert_eq!((*summarized_messages, *kept_messages), (8, 1));
        assert!(tokens_after < tokens_before);
        assert_eq!(
            h,
            vec![
                Message::user_text(format!("{SUMMARY_PREFIX}- summary #1")),
                Message::assistant_text(SUMMARY_ACK),
                Message::user_text("third task: summary slide"),
            ]
        );
        // The summarizer saw the older turns as text, with long results cut.
        let segments = f.segments.lock().unwrap();
        let seg = &segments[0];
        assert!(seg.contains("USER: first task: revenue slide"));
        assert!(seg.contains("ASSISTANT CALL refs_read {\"cite\":\"t1\"}"));
        assert!(seg.contains(&format!("RESULT {}\n", "r".repeat(1500))));
        assert!(!seg.contains(&"r".repeat(1501)));
        assert!(seg.contains("ASSISTANT: cost slide done"));
        assert!(!seg.contains("third task"));
        // The event replays to the same history.
        assert!(matches!(
            out.event(),
            Some(Event::Compaction {
                kept_messages: 1,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn async_closures_are_summarizers() {
        let mut h = three_turns(4000);
        let cfg = CompactionConfig::new(2000);
        let est = ContextEstimate::default();
        let summarizer = |segment: String| async move {
            Ok::<_, anyhow::Error>(format!("{} chars summarized", segment.len()))
        };
        let out = maybe_compact(&mut h, &cfg, &est, &summarizer).await;
        assert!(out.is_compacted());
        assert!(h[0].text().ends_with("chars summarized"));
    }

    #[tokio::test]
    async fn failed_or_empty_summary_keeps_history() {
        let mut h = three_turns(4000);
        let before = h.clone();
        let cfg = CompactionConfig::new(2000);
        let est = ContextEstimate::default();
        let failing = |_s: String| async { Err::<String, _>(anyhow!("endpoint down")) };
        let out = maybe_compact(&mut h, &cfg, &est, &failing).await;
        assert_eq!(out, CompactionOutcome::Failed("endpoint down".into()));
        assert!(out.event().is_none());
        let empty = |_s: String| async { Ok::<_, anyhow::Error>("  \n".to_string()) };
        assert!(matches!(
            maybe_compact(&mut h, &cfg, &est, &empty).await,
            CompactionOutcome::Failed(_)
        ));
        assert_eq!(h, before);
    }

    #[tokio::test]
    async fn one_long_turn_keeps_the_last_call_with_its_results() {
        let f = fake();
        // A single turn with three large tool rounds: there is no earlier user message to
        // cut at, and the results of the last call must stay with their call.
        let mut h = vec![Message::user_text("build the whole deck")];
        h.extend(tool_round("t1", 12_000));
        h.extend(tool_round("t2", 12_000));
        h.extend(tool_round("t3", 12_000));
        let cfg = CompactionConfig::new(4000);
        let est = ContextEstimate::default();
        let out = maybe_compact(&mut h, &cfg, &est, &f).await;
        assert!(matches!(
            out,
            CompactionOutcome::Compacted {
                summarized_messages: 5,
                kept_messages: 2,
                ..
            }
        ));
        assert_eq!(h.len(), 3);
        assert!(h[0].text().starts_with(SUMMARY_PREFIX));
        // No acknowledgement: the tail starts with the assistant message.
        assert!(matches!(&h[1].content[..], [ContentPart::ToolCall { id, .. }] if id == "t3"));
        assert!(
            matches!(&h[2].content[..], [ContentPart::ToolResult { tool_call_id, .. }] if tool_call_id == "t3")
        );
        assert!(f.segments.lock().unwrap()[0].contains("USER: build the whole deck"));

        // Still over budget (one result is larger than the budget), but only the summary
        // is older than the tail: no further summarizer call.
        let out = maybe_compact(&mut h, &cfg, &est, &f).await;
        assert_eq!(out, CompactionOutcome::NothingToCompact);
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_turn_over_budget_falls_back_to_the_last_call() {
        let f = fake();
        let mut h = vec![
            Message::user_text("old task"),
            Message::assistant_text("old answer"),
            Message::user_text("big task"),
        ];
        h.extend(tool_round("t1", 8000));
        h.extend(tool_round("t2", 8000));
        let cfg = CompactionConfig::new(4000);
        let out = maybe_compact(&mut h, &cfg, &ContextEstimate::default(), &f).await;
        // Cutting at "big task" would keep 4000 tokens of results; the last round is kept instead.
        assert!(matches!(
            out,
            CompactionOutcome::Compacted {
                summarized_messages: 5,
                kept_messages: 2,
                ..
            }
        ));
        assert!(f.segments.lock().unwrap()[0].contains("USER: big task"));
    }

    #[tokio::test]
    async fn repeated_compaction_folds_the_previous_summary() {
        let f = fake();
        let cfg = CompactionConfig::new(2000);
        let est = ContextEstimate::default();
        let mut h = three_turns(4000);
        assert!(maybe_compact(&mut h, &cfg, &est, &f).await.is_compacted());
        // The third turn runs, then a fourth user message arrives.
        h.extend(tool_round("t3", 8000));
        h.push(Message::assistant_text("summary slide done"));
        h.push(Message::user_text("fourth task"));
        let out = maybe_compact(&mut h, &cfg, &est, &f).await;
        assert!(out.is_compacted());
        assert_eq!(
            h,
            vec![
                Message::user_text(format!("{SUMMARY_PREFIX}- summary #2")),
                Message::assistant_text(SUMMARY_ACK),
                Message::user_text("fourth task"),
            ]
        );
        let segments = f.segments.lock().unwrap();
        assert!(segments[1].contains("USER: [Summary of earlier conversation]\n- summary #1"));
        assert!(segments[1].contains("USER: third task"));
    }

    #[test]
    fn known_usage_replaces_the_heuristic() {
        let h = three_turns(4000);
        let heuristic = ContextEstimate::heuristic(&"s".repeat(400));
        assert_eq!(heuristic.overhead_tokens, 100);
        assert_eq!(heuristic.tokens(&h), 100 + history_tokens(&h));
        // The endpoint reported 500 input tokens when the history had 8 messages: only the
        // ninth message is estimated.
        let known = heuristic.with_usage(500, 8);
        assert_eq!(known.tokens(&h), 500 + message_tokens(&h[8]));
        let cfg = CompactionConfig::new(2000);
        assert!(needs_compaction(&h, &cfg, &heuristic));
        assert!(!needs_compaction(&h, &cfg, &known));
        // A stale length (the history was compacted since) falls back to the heuristic.
        assert_eq!(
            heuristic.with_usage(500, 99).tokens(&h),
            heuristic.tokens(&h)
        );
        assert_eq!(cfg.budget(), 1400);
        // Token estimates per part.
        let m = Message {
            role: Role::User,
            content: vec![
                ContentPart::Text {
                    text: "abcdefgh".into(),
                },
                ContentPart::Image {
                    media_type: "image/png".into(),
                    data: "A".repeat(1200),
                },
            ],
        };
        assert_eq!(message_tokens(&m), 2 + 100 + 4);
    }

    #[tokio::test]
    async fn model_summarizer_sends_the_compaction_request() {
        let client = Arc::new(ScriptedModelClient::new(vec![
            vec![ModelEvent::Text {
                delta: "- decided X".into(),
            }],
            vec![ModelEvent::Stop {
                reason: StopReason::Error,
                message: Some("boom".into()),
            }],
        ]));
        let s = ModelSummarizer::new(client.clone());
        let mut h = three_turns(4000);
        let cfg = CompactionConfig::new(2000);
        let est = ContextEstimate::default();
        assert!(maybe_compact(&mut h, &cfg, &est, &s).await.is_compacted());
        assert_eq!(h[0].text(), format!("{SUMMARY_PREFIX}- decided X"));
        let req = &client.recorded()[0];
        assert_eq!(req.system.as_deref(), Some(SUMMARIZER_SYSTEM));
        assert!(req.tools.is_empty());
        assert!(req.messages[0]
            .text()
            .starts_with("Summarize this conversation segment:\n\nUSER: first task"));
        // A model error keeps the history.
        let mut h2 = three_turns(4000);
        let before = h2.clone();
        let out = maybe_compact(&mut h2, &cfg, &est, &s).await;
        assert_eq!(out, CompactionOutcome::Failed("model error: boom".into()));
        assert_eq!(h2, before);
    }

    #[tokio::test]
    async fn compaction_event_replays_to_the_live_history() {
        // The transcript of two turns and the start of a third.
        let mut events = vec![
            Event::user("first task"),
            Event::tool_call("t1", "refs.read", json!({"cite": "a"})),
            Event::tool_result("t1", true, json!({"text": "r".repeat(9000)}), false),
            Event::assistant("first done"),
            Event::user("second task"),
        ];
        let opts = ReplayOptions {
            close_open_turn: false,
            ..Default::default()
        };
        let mut live = to_messages(&events, &opts);
        let cfg = CompactionConfig::new(2000);
        let out = maybe_compact(&mut live, &cfg, &ContextEstimate::default(), &fake()).await;
        events.push(out.event().unwrap());
        // The turn goes on after the compaction.
        events.push(Event::assistant("second done"));
        live.push(Message::assistant_text("second done"));
        assert_eq!(to_messages(&events, &opts), live);
        assert_eq!(live.len(), 4);
    }
}
