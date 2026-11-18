//! Shared-to-be crate `wb-transcript` (BUILD-SPEC section 4). Must not depend on Present-specific crates.
//!
//! Conversation transcripts for the agent loop:
//!
//! - [`Event`]: one transcript line (Analyst PROTOCOL 5, Present PROTOCOL 6.2), with a
//!   passthrough for app-specific event types.
//! - [`TranscriptStore`]: `conversations/<id>.jsonl`, append and tolerant read, conversation
//!   list, pinned facts.
//! - [`replay`]: rebuild the model chat history from a transcript.
//! - [`ResultCap`]: the 16 KiB tool result cap with spill to an artifacts directory.
//! - [`compact`]: summarize older turns when the context nears the window.

pub mod cap;
pub mod compact;
pub mod event;
pub mod pinned;
pub mod replay;
pub mod staged;
pub mod store;
pub mod util;

pub use cap::{Capped, ResultCap, TOOL_RESULT_CAP_BYTES};
pub use compact::{
    maybe_compact, needs_compaction, CompactionConfig, CompactionOutcome, ContextEstimate,
    ModelSummarizer, Summarizer,
};
pub use event::{DecidedBy, Event, ProposalDecision};
pub use pinned::PinnedFacts;
pub use replay::{ensure_ready_for_user, to_messages, ReplayOptions};
pub use store::{
    append_jsonl, read_jsonl, read_jsonl_lenient, ConversationSummary, ReadReport, TranscriptStore,
};
pub use util::{estimate_tokens, now_rfc3339, stamped_id, truncate_bytes};
