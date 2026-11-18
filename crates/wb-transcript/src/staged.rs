// Moved from Personal Analyst `crates/core/src/agent/mod.rs` as it stood after the security
// review (REVIEW A1, B3, B4), so that both workbenches share the reviewed behaviour.
//! Staged compaction and history repair.
//!
//! [`compact_staged`] brings a history under a token budget in three stages, each tried only
//! if the previous one was not enough:
//!
//! 1. shrink old tool results and large tool-call arguments in place;
//! 2. replace the older part of the history with a summary. The cut is only ever made where
//!    no tool call is separated from its result (B3), it may fall inside the current turn,
//!    and when the summary call fails a mechanical digest is used instead (B4);
//! 3. hard-truncate what is left.
//!
//! The history is valid for both wire formats after every stage. [`repair_history`] makes
//! any history valid: roles alternate, every tool call is answered, nothing is orphaned.

use crate::compact::{history_tokens, message_tokens, Summarizer, SUMMARY_ACK, SUMMARY_PREFIX};
use crate::replay::INTERRUPTED_RESULT;
use crate::util::truncate_bytes;
use serde_json::Value;
use wb_providers::{normalize_messages, ContentPart, Message, Role};

/// What [`compact_staged`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedOutcome {
    /// The history was within the budget.
    NotNeeded,
    /// Old tool results and arguments were shortened; that was enough.
    Shrunk,
    /// The older part was replaced by a summary (from the summarizer, or a mechanical
    /// digest when it failed). `kept_messages` trailing messages were kept verbatim.
    Summarized {
        summary: String,
        mechanical: bool,
        kept_messages: usize,
    },
    /// Nothing could be summarized (or not again yet); what is left was hard-truncated.
    Truncated,
}

/// Tokens the history may use: `fraction` of the window minus the fixed overhead (system
/// prompt and tool definitions), but never less than a tenth of the window.
pub fn history_budget(context_window: u32, fraction: f32, overhead_tokens: usize) -> usize {
    let window = context_window as usize;
    let total = (window as f32 * fraction) as usize;
    total.saturating_sub(overhead_tokens).max(window / 10)
}

/// Render the part of a history to be summarized, capped so the summary request itself
/// fits: at most half of `context_window`, keeping the start (the task) and the end (the
/// latest state).
pub fn render_capped(older: &[Message], context_window: u32) -> String {
    let mut transcript = crate::compact::render_segment(older);
    let cap = (context_window as usize * 2).max(2000);
    if transcript.len() > cap {
        let head = truncate_bytes(&transcript, cap / 3).to_string();
        let tail = tail_bytes(&transcript, cap * 2 / 3);
        transcript = format!("{head}\n[... middle omitted ...]\n{tail}");
    }
    transcript
}

fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Bring `history` under `budget` tokens (see the module documentation). `floor` is the
/// caller's memory between calls: the history size after the last compaction. The older
/// part is not summarized again until the history has grown meaningfully past it, so an
/// over-full context does not cost a summary call at every step (B4).
pub async fn compact_staged(
    history: &mut Vec<Message>,
    budget: usize,
    context_window: u32,
    floor: &mut usize,
    summarizer: &dyn Summarizer,
) -> StagedOutcome {
    if history_tokens(history) <= budget {
        return StagedOutcome::NotNeeded;
    }
    shrink_old_parts(history, 4, 1500);
    if history_tokens(history) <= budget {
        return StagedOutcome::Shrunk;
    }
    let mut outcome = StagedOutcome::Truncated;
    let grown = history_tokens(history) > *floor + (budget / 5).max(100);
    if grown {
        if let Some(split) = choose_split(history, budget / 2) {
            let older: Vec<Message> = history[..split].to_vec();
            let rendered = render_capped(&older, context_window);
            let (summary, mechanical) = match summarizer.summarize(rendered).await {
                Ok(s) if !s.trim().is_empty() => (s, false),
                _ => {
                    tracing::warn!("compaction summary failed; using a mechanical digest");
                    (mechanical_digest(&older), true)
                }
            };
            let tail: Vec<Message> = history[split..].to_vec();
            let mut new_history = vec![Message::user_text(format!(
                "{SUMMARY_PREFIX}{}",
                summary.trim()
            ))];
            if tail.first().map(|m| m.role) != Some(Role::Assistant) {
                new_history.push(Message::assistant_text(SUMMARY_ACK));
            }
            let kept_messages = tail.len();
            new_history.extend(tail);
            *history = new_history;
            outcome = StagedOutcome::Summarized {
                summary: summary.trim().to_string(),
                mechanical,
                kept_messages,
            };
        }
    }
    if history_tokens(history) > budget {
        for limit in [2000usize, 600, 200] {
            shrink_old_parts(history, 0, limit);
            if history_tokens(history) <= budget {
                break;
            }
        }
    }
    *floor = history_tokens(history);
    outcome
}

fn has_tool_results(m: &Message) -> bool {
    m.content
        .iter()
        .any(|p| matches!(p, ContentPart::ToolResult { .. }))
}

/// Whether the history may be cut just before index `i`: the message there must not carry
/// tool results, because those belong to the calls in the message before it.
pub fn is_safe_boundary(history: &[Message], i: usize) -> bool {
    i > 0 && i < history.len() && !has_tool_results(&history[i])
}

/// Where to cut for summarising: the earliest safe boundary whose tail fits `tail_budget`
/// tokens, else the latest safe boundary there is. `None` when no safe cut exists.
pub fn choose_split(history: &[Message], tail_budget: usize) -> Option<usize> {
    let mut suffix = vec![0usize; history.len() + 1];
    for i in (0..history.len()).rev() {
        suffix[i] = suffix[i + 1] + message_tokens(&history[i]);
    }
    let safe: Vec<usize> = (1..history.len())
        .filter(|i| is_safe_boundary(history, *i))
        .collect();
    safe.iter()
        .copied()
        .find(|i| suffix[*i] <= tail_budget)
        .or_else(|| safe.last().copied())
}

pub fn elide(text: &str, limit: usize) -> Option<String> {
    if text.len() <= limit + 80 {
        return None;
    }
    Some(format!(
        "{}\n[... {} bytes elided to save context; re-run the tool or re-read the file if you need them]",
        truncate_bytes(text, limit),
        text.len() - limit
    ))
}

fn elide_value(v: &mut Value, limit: usize) {
    match v {
        Value::String(s) => {
            if let Some(short) = elide(s, limit) {
                *s = short;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| elide_value(x, limit)),
        Value::Object(m) => m.values_mut().for_each(|x| elide_value(x, limit)),
        _ => {}
    }
}

/// Shorten tool results and large string arguments in every message
/// except the last `keep_recent`. Structure (roles, call ids, pairing) is untouched.
pub fn shrink_old_parts(history: &mut [Message], keep_recent: usize, limit: usize) {
    let n = history.len().saturating_sub(keep_recent);
    for m in &mut history[..n] {
        for p in &mut m.content {
            match p {
                ContentPart::ToolResult { content, .. } => {
                    if let Some(short) = elide(content, limit) {
                        *content = short;
                    }
                }
                ContentPart::ToolCall { arguments, .. } => elide_value(arguments, limit),
                ContentPart::Text { text } => {
                    // Text is shortened only when hard-truncating everything.
                    if keep_recent == 0 {
                        if let Some(short) = elide(text, limit) {
                            *text = short;
                        }
                    }
                }
                ContentPart::Image { .. } | ContentPart::Document { .. } => {}
            }
        }
    }
}

/// A summary made without the model, for when the summary call fails: the first request and
/// the tools that were used.
pub fn mechanical_digest(older: &[Message]) -> String {
    let first = older
        .iter()
        .find(|m| m.role == Role::User && !m.text().trim().is_empty())
        .map(|m| truncate_bytes(m.text().trim(), 600).to_string())
        .unwrap_or_default();
    let mut tools: Vec<String> = Vec::new();
    for m in older {
        for p in &m.content {
            if let ContentPart::ToolCall {
                name, arguments, ..
            } = p
            {
                let target = arguments
                    .get("path")
                    .or_else(|| arguments.get("script"))
                    .and_then(Value::as_str)
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default();
                tools.push(format!("{name}{target}"));
            }
        }
    }
    let last = older
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant && !m.text().trim().is_empty())
        .map(|m| truncate_bytes(m.text().trim(), 600).to_string())
        .unwrap_or_default();
    let mut shown = tools.clone();
    shown.truncate(60);
    format!(
        "(An automatic summary could not be produced; {} earlier messages were dropped to fit the context window. Pinned facts in the system prompt still apply; re-read files rather than relying on memory.)\nFirst request: {first}\nTool calls so far ({}): {}\nLast assistant message: {last}",
        older.len(),
        tools.len(),
        shown.join(", ")
    )
}

/// Make a history acceptable to both providers:
///
/// - whitespace-only text is dropped and adjacent same-role messages are merged;
/// - every assistant tool call is answered in the next message (missing results are filled
///   in as errors), and tool results without a call are removed;
/// - the first message is a user message, and the last is an assistant message, so the next
///   user message can follow (`closing_note` is used when one has to be added).
pub fn repair_history(history: &mut Vec<Message>, closing_note: &str) {
    let mut msgs = normalize_messages(history);
    let mut out: Vec<Message> = Vec::with_capacity(msgs.len() + 2);
    let mut i = 0;
    while i < msgs.len() {
        let m = msgs[i].clone();
        match m.role {
            Role::Assistant => {
                let ids: Vec<String> = m
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::ToolCall { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                out.push(m);
                if ids.is_empty() {
                    i += 1;
                    continue;
                }
                // The answering message: results for exactly these ids first, then the rest.
                let mut next = match msgs.get(i + 1) {
                    Some(n) if n.role == Role::User => {
                        i += 1;
                        std::mem::replace(
                            &mut msgs[i],
                            Message {
                                role: Role::User,
                                content: vec![],
                            },
                        )
                    }
                    _ => Message {
                        role: Role::User,
                        content: vec![],
                    },
                };
                let mut answered: Vec<ContentPart> = Vec::new();
                for id in &ids {
                    let pos = next.content.iter().position(
                        |p| matches!(p, ContentPart::ToolResult { tool_call_id, .. } if tool_call_id == id),
                    );
                    answered.push(match pos {
                        Some(p) => next.content.remove(p),
                        None => ContentPart::ToolResult {
                            tool_call_id: id.clone(),
                            content: INTERRUPTED_RESULT.into(),
                            is_error: true,
                        },
                    });
                }
                next.content
                    .retain(|p| !matches!(p, ContentPart::ToolResult { .. }));
                answered.extend(next.content);
                out.push(Message {
                    role: Role::User,
                    content: answered,
                });
                i += 1;
            }
            Role::User => {
                // No assistant call precedes this message: any tool result here is orphaned.
                let content: Vec<ContentPart> = m
                    .content
                    .into_iter()
                    .filter(|p| !matches!(p, ContentPart::ToolResult { .. }))
                    .collect();
                if !content.is_empty() {
                    out.push(Message {
                        role: Role::User,
                        content,
                    });
                }
                i += 1;
            }
        }
    }
    let mut out = normalize_messages(&out);
    if out.first().map(|m| m.role) == Some(Role::Assistant) {
        out.insert(0, Message::user_text("(earlier conversation omitted)"));
    }
    if out.last().map(|m| m.role) == Some(Role::User) {
        out.push(Message::assistant_text(closing_note));
    }
    *history = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, bytes: usize) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall {
                id: id.into(),
                name: "fs_read".into(),
                arguments: json!({"path": "a.py", "pad": "x".repeat(bytes)}),
            }],
        }
    }

    fn result(id: &str, bytes: usize) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentPart::ToolResult {
                tool_call_id: id.into(),
                content: "r".repeat(bytes),
                is_error: false,
            }],
        }
    }

    /// One long turn: a user message, then call/result pairs only.
    fn long_turn(pairs: usize, bytes: usize) -> Vec<Message> {
        let mut h = vec![Message::user_text("do the task")];
        for i in 0..pairs {
            h.push(call(&format!("c{i}"), 20));
            h.push(result(&format!("c{i}"), bytes));
        }
        h
    }

    fn valid(history: &[Message]) {
        assert_eq!(history[0].role, Role::User);
        for w in history.windows(2) {
            assert_ne!(w[0].role, w[1].role, "roles alternate");
        }
        for (i, m) in history.iter().enumerate() {
            let results: Vec<&String> = m
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ToolResult { tool_call_id, .. } => Some(tool_call_id),
                    _ => None,
                })
                .collect();
            if !results.is_empty() {
                let prev = &history[i - 1];
                for id in results {
                    assert!(
                        prev.content.iter().any(
                            |p| matches!(p, ContentPart::ToolCall { id: cid, .. } if cid == id)
                        ),
                        "result {id} follows its call"
                    );
                }
            }
        }
    }

    /// B3: with no plain user message to split at, the cut must not orphan a tool result.
    #[tokio::test]
    async fn the_cut_never_separates_a_call_from_its_result() {
        let mut h = long_turn(12, 4000);
        let before = history_tokens(&h);
        let mut floor = 0;
        let summarizer =
            |_seg: String| async { Ok::<_, anyhow::Error>("- did things".to_string()) };
        // Too small for stage 1 alone.
        let out = compact_staged(&mut h, 1500, 32768, &mut floor, &summarizer).await;
        assert!(
            matches!(
                out,
                StagedOutcome::Summarized {
                    mechanical: false,
                    ..
                }
            ),
            "{out:?}"
        );
        valid(&h);
        assert!(h[0].text().starts_with(SUMMARY_PREFIX));
        assert!(history_tokens(&h) < before);
        assert_eq!(floor, history_tokens(&h));
    }

    /// B4: a failing summarizer does not leave the history over budget, and the next step
    /// does not ask for another summary until the history has grown.
    #[tokio::test]
    async fn a_failed_summary_falls_back_and_is_not_retried_every_step() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let failing = |_seg: String| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<String, _>(anyhow::anyhow!("HTTP 529")) }
        };
        let mut h = long_turn(12, 4000);
        let mut floor = 0;
        let out = compact_staged(&mut h, 1500, 32768, &mut floor, &failing).await;
        assert!(
            matches!(
                out,
                StagedOutcome::Summarized {
                    mechanical: true,
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(h[0]
            .text()
            .contains("automatic summary could not be produced"));
        valid(&h);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // One more small step: still over a tiny budget, but no new summary call.
        h.push(call("z", 20));
        h.push(result("z", 50));
        let out = compact_staged(&mut h, 200, 32768, &mut floor, &failing).await;
        assert_eq!(out, StagedOutcome::Truncated);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        valid(&h);
    }

    #[tokio::test]
    async fn within_budget_nothing_changes_and_old_results_shrink_first() {
        let never = |_seg: String| async { Err::<String, _>(anyhow::anyhow!("not called")) };
        let mut h = long_turn(2, 100);
        let same = h.clone();
        let mut floor = 0;
        assert_eq!(
            compact_staged(&mut h, 100_000, 32768, &mut floor, &never).await,
            StagedOutcome::NotNeeded
        );
        assert_eq!(h, same);
        // Old results are large, recent ones small: shrinking the old ones is enough.
        let mut h = long_turn(6, 8000);
        let budget = history_tokens(&h) / 2;
        assert_eq!(
            compact_staged(&mut h, budget, 32768, &mut floor, &never).await,
            StagedOutcome::Shrunk
        );
        assert_eq!(h.len(), same.len() + 8);
        valid(&h);
    }

    #[test]
    fn budget_never_drops_below_a_tenth_of_the_window() {
        assert_eq!(history_budget(100_000, 0.7, 10_000), 60_000);
        assert_eq!(history_budget(100_000, 0.7, 90_000), 10_000);
    }

    /// A1: any history becomes one both wire formats accept.
    #[test]
    fn repair_answers_calls_and_drops_orphans() {
        let mut h = vec![
            result("orphan", 10),
            Message::user_text("task"),
            Message::user_text("more"),
            call("c1", 10),
        ];
        repair_history(&mut h, "(closed)");
        valid(&h);
        assert_eq!(h[0].text(), "task\nmore");
        assert!(matches!(
            &h[2].content[0],
            ContentPart::ToolResult { tool_call_id, is_error: true, .. } if tool_call_id == "c1"
        ));
        assert_eq!(h.last().unwrap().text(), "(closed)");
        let mut empty: Vec<Message> = vec![];
        repair_history(&mut empty, "x");
        assert!(empty.is_empty());
    }
}
