// Ported from Fibration Lab `crates/protocol/src/transcript.rs` at commit f229e4d.
// Adapted: the union of Analyst PROTOCOL 5 and Present PROTOCOL 6.2. `proposal.kind` is a
// string each app defines, `summary` carries free-form details, `snapshot` and `compaction`
// are added, and event types this crate does not know pass through as raw JSON.
//! Transcript events, `conversations/<id>.jsonl`.

use crate::util::now_rfc3339;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Outcome recorded for a proposal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ProposalDecision {
    Accept,
    Reject,
    Auto,
}

impl ProposalDecision {
    pub fn is_accepted(self) -> bool {
        matches!(self, ProposalDecision::Accept | ProposalDecision::Auto)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum DecidedBy {
    User,
    Policy,
}

/// One transcript line. `ts` is RFC 3339 UTC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    User {
        ts: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachments: Option<Vec<String>>,
    },
    Assistant {
        ts: String,
        text: String,
    },
    ToolCall {
        ts: String,
        id: String,
        /// Internal tool name (`fs.read`, `slide.propose_update`).
        name: String,
        arguments: Value,
    },
    ToolResult {
        ts: String,
        id: String,
        ok: bool,
        result: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
    },
    Proposal {
        ts: String,
        id: String,
        /// App-defined kind (`edit`, `create`, `slide-update`, `skill-script`, ...).
        kind: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        /// Ids or paths the proposal changes. Reads Present's `slides` key too.
        #[serde(default, alias = "slides", skip_serializing_if = "Option::is_none")]
        targets: Option<Vec<String>>,
        /// The deck a deck change belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deck: Option<String>,
        /// The changed document before and after, for proposals that keep their shape
        /// (slides).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<serde_json::Value>,
    },
    Decision {
        ts: String,
        proposal_id: String,
        decision: ProposalDecision,
        by: DecidedBy,
        /// The user edited the proposal before accepting it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modified: Option<bool>,
    },
    /// Analyst: a script run started.
    Run {
        ts: String,
        run_id: String,
        script: String,
        args: Vec<String>,
        target: String,
    },
    /// Analyst: git checkpoint ref.
    Checkpoint {
        ts: String,
        r#ref: String,
    },
    /// Present: snapshot taken (replaces `checkpoint`).
    Snapshot {
        ts: String,
        snapshot_id: String,
        reason: String,
    },
    Usage {
        ts: String,
        input_tokens: u64,
        output_tokens: u64,
        model: String,
    },
    /// Turn summary. `details` holds the app's fields at the top level of the JSON object
    /// (Analyst: `files_changed`, `runs`, `figures`; Present: `slides_changed`, `exports`).
    Summary {
        ts: String,
        text: String,
        #[serde(flatten)]
        details: Map<String, Value>,
    },
    /// Older history was replaced by a summary. `kept_messages` is the number of trailing
    /// chat messages kept verbatim after the summary; `replay` uses it to rebuild the same
    /// history.
    Compaction {
        ts: String,
        text: String,
        kept_messages: usize,
    },
    /// Any other event: a JSON object with at least `type`, kept as it is (Present: `render`,
    /// `check`, `skill_loaded`, `export`).
    #[serde(untagged)]
    Other(Value),
}

impl Event {
    pub fn user(text: impl Into<String>) -> Event {
        Event::User {
            ts: now_rfc3339(),
            text: text.into(),
            attachments: None,
        }
    }

    pub fn user_with_attachments(text: impl Into<String>, attachments: Vec<String>) -> Event {
        Event::User {
            ts: now_rfc3339(),
            text: text.into(),
            attachments: (!attachments.is_empty()).then_some(attachments),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Event {
        Event::Assistant {
            ts: now_rfc3339(),
            text: text.into(),
        }
    }

    pub fn tool_call(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Event {
        Event::ToolCall {
            ts: now_rfc3339(),
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    pub fn tool_result(id: impl Into<String>, ok: bool, result: Value, truncated: bool) -> Event {
        Event::ToolResult {
            ts: now_rfc3339(),
            id: id.into(),
            ok,
            result,
            truncated: truncated.then_some(true),
        }
    }

    pub fn proposal(
        id: impl Into<String>,
        kind: impl Into<String>,
        summary: impl Into<String>,
        diff: Option<String>,
        targets: Option<Vec<String>>,
    ) -> Event {
        Event::Proposal {
            ts: now_rfc3339(),
            id: id.into(),
            kind: kind.into(),
            summary: summary.into(),
            diff,
            targets,
            deck: None,
            before: None,
            after: None,
        }
    }

    pub fn decision(
        proposal_id: impl Into<String>,
        decision: ProposalDecision,
        by: DecidedBy,
    ) -> Event {
        Event::Decision {
            ts: now_rfc3339(),
            proposal_id: proposal_id.into(),
            decision,
            by,
            modified: None,
        }
    }

    pub fn checkpoint(r#ref: impl Into<String>) -> Event {
        Event::Checkpoint {
            ts: now_rfc3339(),
            r#ref: r#ref.into(),
        }
    }

    pub fn snapshot(snapshot_id: impl Into<String>, reason: impl Into<String>) -> Event {
        Event::Snapshot {
            ts: now_rfc3339(),
            snapshot_id: snapshot_id.into(),
            reason: reason.into(),
        }
    }

    pub fn usage(input_tokens: u64, output_tokens: u64, model: impl Into<String>) -> Event {
        Event::Usage {
            ts: now_rfc3339(),
            input_tokens,
            output_tokens,
            model: model.into(),
        }
    }

    /// `details` must be a JSON object (anything else is stored under `details`); the keys
    /// `ts`, `type`, and `text` are reserved and dropped.
    pub fn summary(text: impl Into<String>, details: Value) -> Event {
        let mut details = match details {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            other => Map::from_iter([("details".to_string(), other)]),
        };
        for reserved in ["ts", "type", "text"] {
            details.remove(reserved);
        }
        Event::Summary {
            ts: now_rfc3339(),
            text: text.into(),
            details,
        }
    }

    pub fn compaction(text: impl Into<String>, kept_messages: usize) -> Event {
        Event::Compaction {
            ts: now_rfc3339(),
            text: text.into(),
            kept_messages,
        }
    }

    /// An app-specific event: `{ts, type: event_type, ...fields}`. `fields` must be a JSON
    /// object (anything else is stored under `value`). Do not use a type name this crate
    /// defines: such a line reads back as that variant when its fields fit.
    pub fn other(event_type: &str, fields: Value) -> Event {
        let mut m = Map::new();
        m.insert("ts".into(), Value::String(now_rfc3339()));
        m.insert("type".into(), Value::String(event_type.to_string()));
        match fields {
            Value::Object(f) => {
                for (k, v) in f {
                    if k != "ts" && k != "type" {
                        m.insert(k, v);
                    }
                }
            }
            Value::Null => {}
            other => {
                m.insert("value".into(), other);
            }
        }
        Event::Other(Value::Object(m))
    }

    pub fn ts(&self) -> &str {
        match self {
            Event::User { ts, .. }
            | Event::Assistant { ts, .. }
            | Event::ToolCall { ts, .. }
            | Event::ToolResult { ts, .. }
            | Event::Proposal { ts, .. }
            | Event::Decision { ts, .. }
            | Event::Run { ts, .. }
            | Event::Checkpoint { ts, .. }
            | Event::Snapshot { ts, .. }
            | Event::Usage { ts, .. }
            | Event::Summary { ts, .. }
            | Event::Compaction { ts, .. } => ts,
            Event::Other(v) => v.get("ts").and_then(Value::as_str).unwrap_or(""),
        }
    }

    /// The `type` tag as written to the file.
    pub fn type_name(&self) -> &str {
        match self {
            Event::User { .. } => "user",
            Event::Assistant { .. } => "assistant",
            Event::ToolCall { .. } => "tool_call",
            Event::ToolResult { .. } => "tool_result",
            Event::Proposal { .. } => "proposal",
            Event::Decision { .. } => "decision",
            Event::Run { .. } => "run",
            Event::Checkpoint { .. } => "checkpoint",
            Event::Snapshot { .. } => "snapshot",
            Event::Usage { .. } => "usage",
            Event::Summary { .. } => "summary",
            Event::Compaction { .. } => "compaction",
            Event::Other(v) => v.get("type").and_then(Value::as_str).unwrap_or(""),
        }
    }

    /// A field of an `Other` event.
    pub fn field(&self, key: &str) -> Option<&Value> {
        match self {
            Event::Other(v) => v.get(key),
            Event::Summary { details, .. } => details.get(key),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_round_trip_with_documented_tags() {
        let events = vec![
            Event::User {
                ts: "t".into(),
                text: "hi".into(),
                attachments: None,
            },
            Event::Assistant {
                ts: "t".into(),
                text: "yo".into(),
            },
            Event::ToolCall {
                ts: "t".into(),
                id: "c1".into(),
                name: "fs.read".into(),
                arguments: json!({"path":"a.py"}),
            },
            Event::ToolResult {
                ts: "t".into(),
                id: "c1".into(),
                ok: true,
                result: json!({"content":"x"}),
                truncated: Some(false),
            },
            Event::Proposal {
                ts: "t".into(),
                id: "p1".into(),
                kind: "edit".into(),
                summary: "edit a.py".into(),
                diff: Some("--- a\n+++ b\n".into()),
                targets: None,
                deck: None,
                before: None,
                after: None,
            },
            Event::Decision {
                ts: "t".into(),
                proposal_id: "p1".into(),
                decision: ProposalDecision::Auto,
                by: DecidedBy::Policy,
                modified: None,
            },
            Event::Run {
                ts: "t".into(),
                run_id: "r".into(),
                script: "s.py".into(),
                args: vec![],
                target: "local".into(),
            },
            Event::Checkpoint {
                ts: "t".into(),
                r#ref: "refs/fibration/checkpoints/x".into(),
            },
            Event::Snapshot {
                ts: "t".into(),
                snapshot_id: "snap-1".into(),
                reason: "turn".into(),
            },
            Event::Usage {
                ts: "t".into(),
                input_tokens: 1,
                output_tokens: 2,
                model: "p/m".into(),
            },
            Event::Summary {
                ts: "t".into(),
                text: "done".into(),
                details: Map::new(),
            },
            Event::Compaction {
                ts: "t".into(),
                text: "sum".into(),
                kept_messages: 3,
            },
        ];
        let tags = [
            "user",
            "assistant",
            "tool_call",
            "tool_result",
            "proposal",
            "decision",
            "run",
            "checkpoint",
            "snapshot",
            "usage",
            "summary",
            "compaction",
        ];
        assert_eq!(events.len(), tags.len());
        for (e, tag) in events.iter().zip(tags) {
            let v = serde_json::to_value(e).unwrap();
            assert_eq!(v["type"], tag);
            assert_eq!(e.type_name(), tag);
            assert_eq!(e.ts(), "t");
            let back: Event = serde_json::from_value(v).unwrap();
            assert_eq!(&back, e);
        }
        let v = serde_json::to_value(&events[7]).unwrap();
        assert_eq!(v["ref"], "refs/fibration/checkpoints/x");
    }

    #[test]
    fn analyst_lines_parse() {
        // Lines as Analyst PROTOCOL 5 writes them.
        let e: Event = serde_json::from_value(json!({
            "ts": "t", "type": "summary", "text": "done",
            "files_changed": ["a.py"], "runs": [], "figures": []
        }))
        .unwrap();
        let Event::Summary { text, details, .. } = &e else {
            panic!("not a summary: {e:?}");
        };
        assert_eq!(text, "done");
        assert_eq!(details["files_changed"], json!(["a.py"]));
        assert_eq!(e.field("runs"), Some(&json!([])));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["files_changed"], json!(["a.py"]));
        assert!(v.get("details").is_none());

        let e: Event = serde_json::from_value(json!({
            "ts": "t", "type": "proposal", "id": "p", "kind": "commit", "summary": "git.commit"
        }))
        .unwrap();
        assert!(
            matches!(&e, Event::Proposal { kind, diff: None, targets: None, .. } if kind == "commit")
        );
        let e: Event = serde_json::from_value(json!({
            "ts": "t", "type": "decision", "proposal_id": "p", "decision": "accept", "by": "user"
        }))
        .unwrap();
        assert!(matches!(
            e,
            Event::Decision {
                decision: ProposalDecision::Accept,
                by: DecidedBy::User,
                ..
            }
        ));
    }

    #[test]
    fn present_lines_parse() {
        // Present PROTOCOL 6.2 additions.
        let e: Event = serde_json::from_value(json!({
            "ts": "t", "type": "proposal", "id": "p1", "kind": "slide-update",
            "summary": "Update s-003", "slides": ["s-003"], "diff": "@@"
        }))
        .unwrap();
        let Event::Proposal { kind, targets, .. } = &e else {
            panic!("not a proposal: {e:?}");
        };
        assert_eq!(kind, "slide-update");
        assert_eq!(targets.as_deref(), Some(&["s-003".to_string()][..]));

        let e: Event = serde_json::from_value(json!({
            "ts": "t", "type": "summary", "text": "Built 2 slides",
            "slides_changed": ["s-001", "s-002"], "exports": []
        }))
        .unwrap();
        assert_eq!(e.field("slides_changed"), Some(&json!(["s-001", "s-002"])));

        // App-specific events pass through unchanged.
        for line in [
            json!({"ts": "t", "type": "render", "out": "r/1.png", "sha256": "ab", "engine": "lo"}),
            json!({"ts": "t", "type": "check", "diagnostics": 2, "errors": 1, "slides": ["s-001"]}),
            json!({"ts": "t", "type": "skill_loaded", "skill": "storyline"}),
            json!({"ts": "t", "type": "export", "format": "pdf", "out": "x.pdf", "snapshot_id": "s"}),
        ] {
            let e: Event = serde_json::from_value(line.clone()).unwrap();
            assert!(matches!(e, Event::Other(_)), "{line}");
            assert_eq!(e.type_name(), line["type"].as_str().unwrap());
            assert_eq!(e.ts(), "t");
            assert_eq!(serde_json::to_value(&e).unwrap(), line);
        }
    }

    #[test]
    fn constructors_stamp_and_shape() {
        let e = Event::other(
            "skill_loaded",
            json!({"skill": "storyline", "type": "x", "ts": "y"}),
        );
        assert_eq!(e.type_name(), "skill_loaded");
        assert_eq!(e.field("skill"), Some(&json!("storyline")));
        assert!(chrono::DateTime::parse_from_rfc3339(e.ts()).is_ok());
        let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);

        let s = Event::summary(
            "done",
            json!({"slides_changed": ["s1"], "text": "dup", "type": "x"}),
        );
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["type"], "summary");
        assert_eq!(v["text"], "done");
        assert_eq!(v["slides_changed"], json!(["s1"]));
        assert_eq!(serde_json::from_value::<Event>(v).unwrap(), s);
        assert_eq!(
            Event::summary("x", json!([1])).field("details"),
            Some(&json!([1]))
        );

        let r = Event::tool_result("c", true, json!(1), false);
        assert!(serde_json::to_value(&r).unwrap().get("truncated").is_none());
        let u = Event::user_with_attachments("hi", vec![]);
        assert!(matches!(
            u,
            Event::User {
                attachments: None,
                ..
            }
        ));
        assert!(ProposalDecision::Auto.is_accepted() && !ProposalDecision::Reject.is_accepted());
    }
}
