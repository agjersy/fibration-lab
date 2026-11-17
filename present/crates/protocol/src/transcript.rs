//! Transcript events, `conversations/<id>.jsonl` (PROTOCOL 6.2).
//!
//! The base events are Personal Analyst's (its PROTOCOL 5; ported from its
//! `crates/protocol/src/transcript.rs`). Differences: `checkpoint` carries `snapshot_id`
//! instead of `ref`; `proposal` and `summary` have Present's fields; `snapshot`, `render`,
//! `check`, `skill_loaded`, and `export` are new.

use crate::ids::{SlideId, SnapshotId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalKind {
    SlideCreate,
    SlideUpdate,
    SlideDelete,
    SlideMove,
    Outline,
    CodeEdit,
    Build,
    GeneratedSlide,
    SkillScript,
    ModeSwitch,
    Export,
}

impl ProposalKind {
    /// Proposals that change slides of the deck document.
    pub fn is_slide_change(self) -> bool {
        matches!(
            self,
            ProposalKind::SlideCreate
                | ProposalKind::SlideUpdate
                | ProposalKind::SlideDelete
                | ProposalKind::SlideMove
        )
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Accept,
    Reject,
    Auto,
}

impl Decision {
    pub fn is_accepted(self) -> bool {
        matches!(self, Decision::Accept | Decision::Auto)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum DecidedBy {
    User,
    Policy,
}

/// One transcript line. `ts` is RFC 3339 UTC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(rename = "TranscriptEvent")]
pub enum Event {
    User {
        ts: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        attachments: Option<Vec<String>>,
    },
    Assistant {
        ts: String,
        text: String,
    },
    ToolCall {
        ts: String,
        id: String,
        name: String,
        #[ts(type = "unknown")]
        arguments: Value,
    },
    ToolResult {
        ts: String,
        id: String,
        ok: bool,
        #[ts(type = "any")]
        result: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        truncated: Option<bool>,
    },
    Proposal {
        ts: String,
        id: String,
        kind: ProposalKind,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        slides: Option<Vec<SlideId>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        diff: Option<String>,
    },
    Decision {
        ts: String,
        proposal_id: String,
        decision: Decision,
        by: DecidedBy,
    },
    /// Analyst's script-run event, kept unchanged.
    Run {
        ts: String,
        run_id: String,
        script: String,
        args: Vec<String>,
        target: String,
    },
    /// Analyst's checkpoint event with `ref` replaced by `snapshot_id`.
    Checkpoint {
        ts: String,
        snapshot_id: SnapshotId,
    },
    Snapshot {
        ts: String,
        snapshot_id: SnapshotId,
        reason: String,
    },
    Render {
        ts: String,
        out: String,
        sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        engine: Option<String>,
    },
    Check {
        ts: String,
        diagnostics: u32,
        errors: u32,
        slides: Vec<SlideId>,
    },
    SkillLoaded {
        ts: String,
        skill: String,
    },
    Export {
        ts: String,
        format: String,
        out: String,
        snapshot_id: SnapshotId,
    },
    Summary {
        ts: String,
        text: String,
        slides_changed: Vec<SlideId>,
        exports: Vec<String>,
    },
    Usage {
        ts: String,
        input_tokens: u64,
        output_tokens: u64,
        model: String,
    },
}

impl Event {
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
            | Event::Render { ts, .. }
            | Event::Check { ts, .. }
            | Event::SkillLoaded { ts, .. }
            | Event::Export { ts, .. }
            | Event::Summary { ts, .. }
            | Event::Usage { ts, .. } => ts,
        }
    }

    /// The `type` tag.
    pub fn type_name(&self) -> &'static str {
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
            Event::Render { .. } => "render",
            Event::Check { .. } => "check",
            Event::SkillLoaded { .. } => "skill_loaded",
            Event::Export { .. } => "export",
            Event::Summary { .. } => "summary",
            Event::Usage { .. } => "usage",
        }
    }
}
