// Ported from Fibration Lab `crates/core/src/agent/mod.rs` at commit f229e4d: the way
// `run_turn` builds `history` (one assistant message per model call holding its text and tool
// calls, one user message holding the results) and `recover_after_cancel`.
// Adapted: Analyst keeps the history in memory only; here the same history is rebuilt from
// the transcript so a conversation can be resumed after a restart, including compactions.
//! Transcript -> model messages.

use crate::cap::{ResultCap, TOOL_RESULT_CAP_BYTES};
use crate::compact::{summary_messages, tail_start};
use crate::event::Event;
use serde_json::Value;
use std::path::PathBuf;
use wb_providers::{names, ContentPart, Message, Role};

/// Tool result content for a call whose result never reached the transcript.
pub const INTERRUPTED_RESULT: &str = "{\"error\":\"interrupted before a result was recorded\"}";
/// Assistant message closing a turn that was cut before the model answered.
pub const CANCELLED_TURN: &str = "(turn cancelled by the user)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOptions {
    /// Map internal tool names to model-facing names (`deck.outline` -> `deck_outline`).
    /// On by default; switch off when the transcript already holds model-facing names.
    pub model_tool_names: bool,
    /// Cap on each replayed tool result (bytes of compact JSON).
    pub max_result_bytes: usize,
    /// Where results over the cap are written in full (`<dir>/<conversation>/<call id>.json`
    /// with `artifact_prefix` as the conversation part). `None`: truncate only.
    pub artifacts_dir: Option<PathBuf>,
    /// First path component of spilled artifacts, normally the conversation id.
    pub artifact_prefix: String,
    /// End the history with an assistant message so that the next user message can be
    /// appended (see `ensure_ready_for_user`). On by default.
    pub close_open_turn: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        ReplayOptions {
            model_tool_names: true,
            max_result_bytes: TOOL_RESULT_CAP_BYTES,
            artifacts_dir: None,
            artifact_prefix: "replay".to_string(),
            close_open_turn: true,
        }
    }
}

struct Builder<'a> {
    opts: &'a ReplayOptions,
    cap: ResultCap,
    messages: Vec<Message>,
    /// Assistant message being assembled (text and tool calls of one model call).
    assistant: Vec<ContentPart>,
    /// Calls of the last flushed assistant message still waiting for a result: (id, name).
    open: Vec<(String, String)>,
    /// Results gathered for the last flushed assistant message, by call id.
    results: Vec<(String, ContentPart)>,
    /// Calls of the assistant message being assembled.
    pending_calls: Vec<(String, String)>,
}

impl Builder<'_> {
    fn tool_name(&self, internal: &str) -> String {
        if self.opts.model_tool_names {
            names::to_model_name(internal)
        } else {
            internal.to_string()
        }
    }

    /// Close the assistant message being assembled; its calls become open.
    fn flush_assistant(&mut self) {
        if self.assistant.is_empty() {
            return;
        }
        self.flush_results();
        let content = std::mem::take(&mut self.assistant);
        self.messages.push(Message {
            role: Role::Assistant,
            content,
        });
        self.open = std::mem::take(&mut self.pending_calls);
    }

    /// Close the result message for the open calls; calls without a result get a synthetic
    /// error so that the history stays well-formed.
    fn flush_results(&mut self) {
        if self.open.is_empty() {
            self.results.clear();
            return;
        }
        let open = std::mem::take(&mut self.open);
        let mut results = std::mem::take(&mut self.results);
        let mut content = Vec::with_capacity(open.len());
        // Results in call order, as providers expect.
        for (id, _) in open {
            let part = match results.iter().position(|(rid, _)| *rid == id) {
                Some(i) => results.remove(i).1,
                None => self.result_part(&id, INTERRUPTED_RESULT.to_string(), true),
            };
            content.push(part);
        }
        self.messages.push(Message {
            role: Role::User,
            content,
        });
    }

    fn result_part(&self, id: &str, content: String, is_error: bool) -> ContentPart {
        ContentPart::ToolResult {
            tool_call_id: id.to_string(),
            content,
            is_error,
        }
    }

    fn user(&mut self, text: &str, attachments: Option<&Vec<String>>) {
        self.flush_assistant();
        self.flush_results();
        if matches!(self.messages.last(), Some(m) if m.role == Role::User) {
            self.messages.push(Message::assistant_text(CANCELLED_TURN));
        }
        let mut text = text.to_string();
        if let Some(a) = attachments.filter(|a| !a.is_empty()) {
            text.push_str(&format!("\n\n[Attachments: {}]", a.join(", ")));
        }
        self.messages.push(Message::user_text(text));
    }

    fn assistant_text(&mut self, text: &str) {
        // Text after tool calls belongs to the next model call.
        if !self.pending_calls.is_empty() {
            self.flush_assistant();
        }
        if self.assistant.is_empty() {
            self.flush_results();
        }
        if let Some(ContentPart::Text { text: prev }) = self.assistant.last_mut() {
            prev.push_str("\n\n");
            prev.push_str(text);
        } else {
            self.assistant.push(ContentPart::Text {
                text: text.to_string(),
            });
        }
    }

    fn tool_call(&mut self, id: &str, name: &str, arguments: &Value) {
        if self.assistant.is_empty() {
            self.flush_results();
        }
        let model_name = self.tool_name(name);
        self.assistant.push(ContentPart::ToolCall {
            id: id.to_string(),
            name: model_name.clone(),
            arguments: arguments.clone(),
        });
        self.pending_calls.push((id.to_string(), model_name));
    }

    fn tool_result(&mut self, id: &str, ok: bool, result: &Value) {
        self.flush_assistant();
        if !self.open.iter().any(|(oid, _)| oid == id) {
            return; // a result without its call (the call was lost): drop it
        }
        if self.results.iter().any(|(rid, _)| rid == id) {
            return;
        }
        let capped = self.cap.cap(
            &format!("{}/{id}", self.opts.artifact_prefix),
            result.clone(),
        );
        let content = serde_json::to_string(&capped.value).unwrap_or_default();
        let part = self.result_part(id, content, !ok);
        self.results.push((id.to_string(), part));
    }

    fn compaction(&mut self, text: &str, kept_messages: usize) {
        self.flush_assistant();
        self.flush_results();
        let len = self.messages.len();
        let start = tail_start(&self.messages, len.saturating_sub(kept_messages));
        let tail = self.messages.split_off(start);
        let starts_with_user = matches!(tail.first(), Some(m) if m.role == Role::User);
        self.messages = summary_messages(text, starts_with_user || tail.is_empty());
        self.messages.extend(tail);
    }
}

/// Rebuild the chat history of a conversation from its transcript.
///
/// - `user` events become user messages; consecutive `assistant` and `tool_call` events of
///   one model call become one assistant message; the `tool_result` events that follow become
///   one user message, in call order.
/// - A `compaction` event replaces everything before its kept tail by the summary.
/// - Tool calls without a recorded result get an error result, results without a call are
///   dropped, and results over the cap are truncated (and spilled when a directory is given).
/// - Other events (proposals, decisions, usage, snapshots, summaries, app events) carry no
///   model context and are skipped.
pub fn to_messages(events: &[Event], opts: &ReplayOptions) -> Vec<Message> {
    let mut b = Builder {
        opts,
        cap: ResultCap {
            max_bytes: opts.max_result_bytes,
            artifacts_dir: opts.artifacts_dir.clone(),
            hint: None,
        },
        messages: Vec::new(),
        assistant: Vec::new(),
        open: Vec::new(),
        results: Vec::new(),
        pending_calls: Vec::new(),
    };
    for e in events {
        match e {
            Event::User {
                text, attachments, ..
            } => b.user(text, attachments.as_ref()),
            Event::Assistant { text, .. } => b.assistant_text(text),
            Event::ToolCall {
                id,
                name,
                arguments,
                ..
            } => b.tool_call(id, name, arguments),
            Event::ToolResult { id, ok, result, .. } => b.tool_result(id, *ok, result),
            Event::Compaction {
                text,
                kept_messages,
                ..
            } => b.compaction(text, *kept_messages),
            _ => {}
        }
    }
    b.flush_assistant();
    b.flush_results();
    let mut messages = b.messages;
    if opts.close_open_turn {
        ensure_ready_for_user(&mut messages);
    }
    messages
}

/// Make a history well-formed again after a turn was cut mid-way (a cancelled turn, a
/// crash): every tool call of the last assistant message gets a result, and a non-empty
/// history ends with an assistant message so that the next user message can follow.
pub fn ensure_ready_for_user(history: &mut Vec<Message>) {
    if let Some(last) = history.last() {
        if last.role == Role::Assistant {
            let pending: Vec<ContentPart> = last
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ToolCall { id, .. } => Some(ContentPart::ToolResult {
                        tool_call_id: id.clone(),
                        content: "{\"error\":\"cancelled by the user\"}".into(),
                        is_error: true,
                    }),
                    _ => None,
                })
                .collect();
            if !pending.is_empty() {
                history.push(Message {
                    role: Role::User,
                    content: pending,
                });
            }
        }
    }
    if matches!(history.last(), Some(m) if m.role != Role::Assistant) {
        history.push(Message::assistant_text(CANCELLED_TURN));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::{SUMMARY_ACK, SUMMARY_PREFIX};
    use serde_json::json;

    fn call(id: &str, name: &str) -> Event {
        Event::tool_call(id, name, json!({"q": id}))
    }
    fn result(id: &str) -> Event {
        Event::tool_result(id, true, json!({"ok": id}), false)
    }
    fn roles(m: &[Message]) -> String {
        m.iter()
            .map(|m| if m.role == Role::User { 'U' } else { 'A' })
            .collect()
    }

    #[test]
    fn replays_turns_with_tool_calls_and_results() {
        let events = vec![
            Event::snapshot("s1", "turn"),
            Event::user("Build a slide"),
            Event::usage(10, 5, "p/m"),
            Event::assistant("Looking at the deck."),
            call("t1", "deck.outline"),
            call("t2", "refs.search"),
            // Results arrive in a different order, with policy events in between.
            Event::tool_result("t2", false, json!({"error": "denied"}), false),
            Event::proposal("p", "slide-create", "new slide", None, None),
            result("t1"),
            call("t3", "slide.propose_create"),
            result("t3"),
            Event::assistant("Done."),
            Event::summary("Done.", json!({"slides_changed": ["s-001"]})),
            Event::other("render", json!({"out": "x.png", "sha256": "ab"})),
            Event::user("Thanks"),
            Event::assistant("You are welcome."),
        ];
        let m = to_messages(&events, &ReplayOptions::default());
        assert_eq!(roles(&m), "UAUAUAUA");
        assert_eq!(m[0], Message::user_text("Build a slide"));
        assert_eq!(
            m[1].content,
            vec![
                ContentPart::Text {
                    text: "Looking at the deck.".into()
                },
                ContentPart::ToolCall {
                    id: "t1".into(),
                    name: "deck_outline".into(),
                    arguments: json!({"q": "t1"})
                },
                ContentPart::ToolCall {
                    id: "t2".into(),
                    name: "refs_search".into(),
                    arguments: json!({"q": "t2"})
                },
            ]
        );
        // Results in call order; the failed one is flagged.
        assert_eq!(
            m[2].content,
            vec![
                ContentPart::ToolResult {
                    tool_call_id: "t1".into(),
                    content: "{\"ok\":\"t1\"}".into(),
                    is_error: false
                },
                ContentPart::ToolResult {
                    tool_call_id: "t2".into(),
                    content: "{\"error\":\"denied\"}".into(),
                    is_error: true
                },
            ]
        );
        assert!(
            matches!(&m[3].content[..], [ContentPart::ToolCall { name, .. }] if name == "slide_propose_create")
        );
        assert!(
            matches!(&m[4].content[..], [ContentPart::ToolResult { tool_call_id, .. }] if tool_call_id == "t3")
        );
        assert_eq!(m[5], Message::assistant_text("Done."));
        assert_eq!(m[6], Message::user_text("Thanks"));
        assert_eq!(m[7], Message::assistant_text("You are welcome."));
    }

    #[test]
    fn internal_names_can_be_kept() {
        let events = vec![Event::user("x"), call("t1", "deck.outline"), result("t1")];
        let opts = ReplayOptions {
            model_tool_names: false,
            close_open_turn: false,
            ..Default::default()
        };
        let m = to_messages(&events, &opts);
        assert_eq!(roles(&m), "UAU");
        assert!(
            matches!(&m[1].content[..], [ContentPart::ToolCall { name, .. }] if name == "deck.outline")
        );
    }

    #[test]
    fn interrupted_turns_are_made_well_formed() {
        // Crash after the calls were recorded: one result missing, one orphan result.
        let events = vec![
            Event::user("go"),
            call("t1", "a.b"),
            call("t2", "a.c"),
            result("t1"),
            result("ghost"),
            result("t1"),
        ];
        let m = to_messages(&events, &ReplayOptions::default());
        assert_eq!(roles(&m), "UAUA");
        assert_eq!(
            m[2].content,
            vec![
                ContentPart::ToolResult {
                    tool_call_id: "t1".into(),
                    content: "{\"ok\":\"t1\"}".into(),
                    is_error: false
                },
                ContentPart::ToolResult {
                    tool_call_id: "t2".into(),
                    content: INTERRUPTED_RESULT.into(),
                    is_error: true
                },
            ]
        );
        assert_eq!(m[3], Message::assistant_text(CANCELLED_TURN));

        // No result at all, then the user writes again.
        let events = vec![
            Event::user("go"),
            call("t1", "a.b"),
            Event::user("hello?"),
            Event::user("anyone?"),
        ];
        let opts = ReplayOptions {
            close_open_turn: false,
            ..Default::default()
        };
        let m = to_messages(&events, &opts);
        assert_eq!(roles(&m), "UAUAUAU");
        assert!(matches!(
            &m[2].content[..],
            [ContentPart::ToolResult { is_error: true, .. }]
        ));
        assert_eq!(m[3], Message::assistant_text(CANCELLED_TURN));
        assert_eq!(m[4], Message::user_text("hello?"));
        assert_eq!(m[5], Message::assistant_text(CANCELLED_TURN));
        assert_eq!(m[6], Message::user_text("anyone?"));

        assert!(to_messages(&[], &ReplayOptions::default()).is_empty());
    }

    #[test]
    fn ensure_ready_closes_open_calls() {
        let mut h = vec![
            Message::user_text("go"),
            Message {
                role: Role::Assistant,
                content: vec![ContentPart::ToolCall {
                    id: "t1".into(),
                    name: "a_b".into(),
                    arguments: json!({}),
                }],
            },
        ];
        ensure_ready_for_user(&mut h);
        assert_eq!(roles(&h), "UAUA");
        assert!(
            matches!(&h[2].content[..], [ContentPart::ToolResult { tool_call_id, is_error: true, .. }] if tool_call_id == "t1")
        );
        let before = h.clone();
        ensure_ready_for_user(&mut h);
        assert_eq!(h, before);
        let mut empty = vec![];
        ensure_ready_for_user(&mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn attachments_and_split_assistant_text() {
        let events = vec![
            Event::user_with_attachments("see", vec!["s-001".into(), "ref:r1#p3".into()]),
            Event::assistant("part one"),
            Event::assistant("part two"),
        ];
        let m = to_messages(&events, &ReplayOptions::default());
        assert_eq!(m[0].text(), "see\n\n[Attachments: s-001, ref:r1#p3]");
        assert_eq!(m[1], Message::assistant_text("part one\n\npart two"));
    }

    #[test]
    fn large_results_are_capped_and_spilled() {
        let dir = tempfile::tempdir().unwrap();
        let events = vec![
            Event::user("x"),
            call("t1", "refs.read"),
            Event::tool_result("t1", true, json!({"text": "z".repeat(50_000)}), false),
        ];
        let opts = ReplayOptions {
            artifacts_dir: Some(dir.path().to_path_buf()),
            artifact_prefix: "c1".into(),
            ..Default::default()
        };
        let m = to_messages(&events, &opts);
        let ContentPart::ToolResult { content, .. } = &m[2].content[0] else {
            panic!("no result");
        };
        assert!(content.len() <= TOOL_RESULT_CAP_BYTES);
        let v: Value = serde_json::from_str(content).unwrap();
        assert_eq!(v["truncated"], true);
        let artifact = dir.path().join("c1").join("t1.json");
        assert_eq!(v["artifact"], artifact.display().to_string());
        assert!(std::fs::metadata(artifact).unwrap().len() > 50_000);

        // Without a directory the result is only truncated.
        let m = to_messages(&events, &ReplayOptions::default());
        let ContentPart::ToolResult { content, .. } = &m[2].content[0] else {
            panic!("no result");
        };
        let v: Value = serde_json::from_str(content).unwrap();
        assert!(v["truncated"] == true && v.get("artifact").is_none());
    }

    #[test]
    fn compaction_event_replaces_older_history() {
        let events = vec![
            Event::user("first task"),
            call("t1", "a.b"),
            result("t1"),
            Event::assistant("did it"),
            Event::user("second task"),
            // Compaction ran before the model call of the second turn, keeping one message.
            Event::compaction("- first task done via a.b", 1),
            Event::assistant("on it"),
        ];
        let m = to_messages(&events, &ReplayOptions::default());
        assert_eq!(roles(&m), "UAUA");
        assert_eq!(
            m[0].text(),
            format!("{SUMMARY_PREFIX}- first task done via a.b")
        );
        assert_eq!(m[1], Message::assistant_text(SUMMARY_ACK));
        assert_eq!(m[2], Message::user_text("second task"));
        assert_eq!(m[3], Message::assistant_text("on it"));
    }

    #[test]
    fn compaction_tail_never_starts_with_orphan_results() {
        let events = vec![
            Event::user("task"),
            call("t1", "a.b"),
            result("t1"),
            call("t2", "a.b"),
            result("t2"),
            // kept_messages = 1 would keep only the results of t2: the tail is widened to
            // include the assistant message that made the call.
            Event::compaction("summary", 1),
            Event::assistant("done"),
        ];
        let m = to_messages(&events, &ReplayOptions::default());
        assert_eq!(roles(&m), "UAUA");
        assert!(m[0].text().starts_with(SUMMARY_PREFIX));
        assert!(matches!(&m[1].content[..], [ContentPart::ToolCall { id, .. }] if id == "t2"));
        assert!(
            matches!(&m[2].content[..], [ContentPart::ToolResult { tool_call_id, .. }] if tool_call_id == "t2")
        );
        assert_eq!(m[3], Message::assistant_text("done"));

        // Everything summarized.
        let events = vec![
            Event::user("task"),
            Event::assistant("ok"),
            Event::compaction("all of it", 0),
        ];
        let m = to_messages(&events, &ReplayOptions::default());
        assert_eq!(roles(&m), "UA");
        assert_eq!(m[1], Message::assistant_text(SUMMARY_ACK));
    }
}
