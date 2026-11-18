//! The agent loop (ARCHITECTURE 5.7): context assembly, modes, a bounded tool loop with policy
//! evaluation and approvals, scope declaration, post-edit lint and contract checks with
//! bounded auto-fix, run failures fed back, turn summaries, and compaction.
//!
//! Invariants this module maintains:
//!
//! - Every way out of a turn (normal end, step limit, model error, recording error) goes
//!   through one finaliser that leaves a history both providers accept, records the turn
//!   summary, and re-baselines the working tree.
//! - Compaction only ever cuts the history where no tool call is separated from its result,
//!   works inside the current turn, and always makes progress even when the summary call
//!   fails.
//! - The declared edit scope lasts one turn, and widening it needs approval.

pub mod approver;
pub mod prompt;

pub use approver::{
    ApprovalDecision, ApprovalRequest, Approver, AutoApprover, RecordingApprover, RejectAllApprover,
};

use crate::checkpoint;
use crate::tools::gate::{Decision as PolicyDecision, Policy, ScopeChange};
use crate::tools::proposals::{self, Proposal};
use crate::tools::registry::{self, ToolClass};
use crate::tools::ToolContext;
use crate::util::{estimate_tokens, now_rfc3339, truncate_bytes};
use analyst_protocol::config::DEFAULT_MAX_OUTPUT_TOKENS;
use analyst_protocol::evals::Mode;
use analyst_protocol::kernel::{Diagnostic, Severity};
use analyst_protocol::transcript::{DecidedBy, Decision, Event, ProposalKind};
use anyhow::{anyhow, Result};
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use wb_providers::model::{
    ChatRequest, ContentPart, Message, ModelCapabilities, ModelEvent, Role, StopReason,
};
use wb_providers::ModelClient;
use wb_transcript::compact::ModelSummarizer;
use wb_transcript::staged::{compact_staged, repair_history};
use wb_transcript::{PinnedFacts, ReplayOptions};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub mode: Mode,
    pub max_steps: u32,
    /// Lint/contract feedback rounds per turn before surfacing to the user (N=3).
    pub auto_fix_iterations: u32,
    /// Compact when the context exceeds this fraction of the window.
    pub compaction_fraction: f32,
    /// `provider/model`, recorded in usage events.
    pub model_name: String,
    pub caps: ModelCapabilities,
    pub checkpoint_keep: usize,
    /// Explicit output token limit per model call. `None` uses the model's configured
    /// `max_output_tokens`, else 16384, never more than half the context window.
    pub max_tokens: Option<u32>,
}

impl AgentConfig {
    pub fn new(mode: Mode, model_name: &str, caps: ModelCapabilities, max_steps: u32) -> Self {
        AgentConfig {
            mode,
            max_steps,
            auto_fix_iterations: 3,
            compaction_fraction: 0.7,
            model_name: model_name.to_string(),
            caps,
            checkpoint_keep: 20,
            max_tokens: None,
        }
    }

    /// The `max_tokens` sent with each request.
    pub fn effective_max_tokens(&self) -> u32 {
        let want = self
            .max_tokens
            .or(self.caps.max_output_tokens)
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
        want.min((self.caps.context_window / 2).max(1024)).max(1)
    }
}

/// What a turn produced.
#[derive(Debug, Clone, Default)]
pub struct TurnSummary {
    pub text: String,
    pub files_changed: Vec<String>,
    pub runs: Vec<String>,
    pub figures: Vec<String>,
    pub steps: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: u32,
    pub tool_errors: u32,
    /// Edits that were applied to a file outside the scope in force when they were decided
    /// (or with no scope declared).
    pub unwanted_edits: u32,
    pub open_issues: Vec<String>,
    pub stopped_by_max_steps: bool,
}

pub type TextSink = Arc<dyn Fn(&str) + Send + Sync>;
pub type EventSink = Arc<dyn Fn(&Event) + Send + Sync>;

pub struct Agent {
    pub model: Arc<dyn ModelClient>,
    pub ctx: ToolContext,
    pub policy: Policy,
    pub approver: Arc<dyn Approver>,
    pub config: AgentConfig,
    history: Vec<Message>,
    pinned: PinnedFacts,
    /// The user message of the turn in progress (skill triggers are matched against it).
    turn_text: String,
    turn_counter: u32,
    project_map: String,
    /// Set while a turn is running; still set afterwards only if the turn future was dropped.
    mid_turn: bool,
    /// Files this agent wrote during the current (or an aborted) turn.
    touched: BTreeSet<String>,
    /// `declare_scope` calls accepted this turn.
    declarations: u32,
    /// History size right after the last summarising compaction (thrash guard).
    compact_floor: usize,
    /// Tool-call ids used so far in this conversation. Providers may repeat ids between
    /// responses; the transcript and the history need them unique.
    used_call_ids: HashSet<String>,
    pub on_text: Option<TextSink>,
    pub on_event: Option<EventSink>,
}

struct Collected {
    text: String,
    calls: Vec<(String, String, Value)>,
    input_tokens: u64,
    output_tokens: u64,
    stop: StopReason,
    message: Option<String>,
}

/// Per-turn working state shared by the loop and the finaliser.
#[derive(Default)]
struct TurnState {
    summary: TurnSummary,
    files_changed: BTreeSet<String>,
    last_text: String,
}

impl Agent {
    pub fn new(
        model: Arc<dyn ModelClient>,
        ctx: ToolContext,
        policy: Policy,
        approver: Arc<dyn Approver>,
        config: AgentConfig,
    ) -> Self {
        Agent {
            model,
            ctx,
            policy,
            approver,
            config,
            history: vec![],
            pinned: PinnedFacts::new(),
            turn_text: String::new(),
            turn_counter: 0,
            project_map: String::new(),
            mid_turn: false,
            touched: BTreeSet::new(),
            declarations: 0,
            compact_floor: 0,
            used_call_ids: HashSet::new(),
            on_text: None,
            on_event: None,
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.config.mode = mode;
        self.policy.set_mode(mode);
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn pinned_facts(&self) -> &[String] {
        self.pinned.facts()
    }

    /// Whether a turn was started and never finalised (its future was dropped).
    pub fn is_mid_turn(&self) -> bool {
        self.mid_turn
    }

    /// Tell the policy which sandbox the kernel reported. `run_turn` does this itself from
    /// `kernel.hello` before the first tool call; callers with other knowledge may set it too.
    pub fn set_sandbox(&mut self, kind: &str) {
        self.policy.set_sandbox(kind);
    }

    /// Make the agent usable again after a turn was aborted mid-way (the desktop app cancels
    /// turns by dropping the future): the history is repaired so both providers accept it,
    /// and the working tree is re-baselined so the next turn does not report the agent's own
    /// edits as the user's. Runs started by the dropped turn cancel themselves (see
    /// `KernelClient::cancel_guard`). Idempotent; `run_turn` also calls it when needed.
    pub fn recover_after_cancel(&mut self) {
        repair_history(&mut self.history, "(turn cancelled by the user)");
        let _ = self.ctx.session.mark_worktree();
        self.touched.clear();
        self.mid_turn = false;
    }

    /// Rebuild the model history, pinned facts, turn counter, and (for a turn that never
    /// finished) the declared scope from a stored transcript, so a reopened conversation
    /// continues where it stopped (A11). Replaces any state the agent had.
    pub fn rehydrate(&mut self, events: &[Event]) {
        let mut pinned = PinnedFacts::new();
        let mut scope: Option<Vec<String>> = None;
        let mut turns = 0u32;
        // Tool calls by id: (model-facing name, arguments).
        let mut calls: std::collections::HashMap<String, (String, Value)> = Default::default();
        for e in events {
            match e {
                Event::User { .. } => {
                    turns += 1;
                    scope = None;
                    pinned.remove_prefixed("Declared scope:");
                    pinned.remove_prefixed("Plan:");
                }
                Event::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => {
                    calls.insert(id.clone(), (name.clone(), arguments.clone()));
                    self.used_call_ids.insert(id.clone());
                }
                Event::ToolResult { id, ok, result, .. } => {
                    let Some((name, args)) = calls.get(id) else {
                        continue;
                    };
                    if *ok {
                        match registry::internal_name(name) {
                            Some("pin_fact") => {
                                if let Some(f) = args.get("fact").and_then(Value::as_str) {
                                    pinned.add(f);
                                }
                            }
                            Some("declare_scope") => {
                                let files = str_array(result.get("scope"));
                                pinned.set_prefixed("Declared scope: ", &files.join(", "));
                                if let Some(plan) = args.get("plan").and_then(Value::as_str) {
                                    pinned.add(format!("Plan: {}", truncate_bytes(plan, 500)));
                                }
                                scope = Some(files);
                            }
                            _ => {}
                        }
                    }
                }
                Event::Summary { files_changed, .. } => {
                    // The turn finished: its scope is gone.
                    scope = None;
                    if !files_changed.is_empty() {
                        pinned.add(format!(
                            "Turn {turns}: changed {}",
                            files_changed.join(", ")
                        ));
                    }
                }
                _ => {}
            }
        }
        // The chat history comes from the shared replay. Notes the agent wrote about its
        // own failures are not model output and are left out.
        let shared: Vec<wb_transcript::Event> = events
            .iter()
            .filter(|e| {
                !matches!(e, Event::Assistant { text, .. }
                    if text.starts_with("[model error]") || text.starts_with("[turn cancelled"))
            })
            .filter_map(|e| crate::session::to_shared(e).ok())
            .collect();
        let options = ReplayOptions {
            // The analysis transcript already holds model-facing tool names, and results
            // were capped when they were recorded.
            model_tool_names: false,
            max_result_bytes: usize::MAX,
            close_open_turn: false,
            ..Default::default()
        };
        let mut history = wb_transcript::to_messages(&shared, &options);
        repair_history(&mut history, "(the previous turn was interrupted)");
        self.history = history;
        self.pinned = pinned;
        self.turn_counter = turns;
        self.policy.set_scope(scope);
        self.mid_turn = false;
        self.touched.clear();
        self.compact_floor = 0;
    }

    fn record(&self, event: Event) -> Result<()> {
        if let Some(cb) = &self.on_event {
            cb(&event);
        }
        self.ctx.session.append_event(&self.ctx.conv_id, &event)
    }

    fn system_prompt(&self) -> String {
        let mut s = String::new();
        s.push_str(prompt::SYSTEM_ROLE);
        s.push_str("\n\n");
        s.push_str(&prompt::contract_rules(&self.ctx.project));
        s.push_str("\n\n");
        s.push_str(prompt::OUTPUT_CONVENTIONS);
        s.push_str("\n\n");
        s.push_str(prompt::mode_instructions(self.config.mode));
        s.push_str("\n\nPOLICY: ");
        s.push_str(&self.policy.summary());
        s.push_str("\n\n");
        s.push_str(&self.project_map);
        if !self.pinned.is_empty() {
            s.push('\n');
            s.push_str(&self.pinned.render_block());
        }
        // Skills: the list by description, and in full those whose triggers match this turn
        // or whose path globs match a file in play (5.16).
        let mut paths: Vec<String> = self.touched.iter().cloned().collect();
        paths.extend(self.policy.scope().unwrap_or_default().iter().cloned());
        let skills = crate::skills::prompt_block(&self.ctx.skills, &self.turn_text, &paths);
        if !skills.is_empty() {
            s.push('\n');
            s.push_str(&skills);
        }
        s
    }

    fn build_request(&self) -> ChatRequest {
        ChatRequest {
            model: self.model.model_id().to_string(),
            system: Some(self.system_prompt()),
            messages: self.history.clone(),
            tools: registry::defs_for_mode(self.config.mode),
            max_tokens: Some(self.config.effective_max_tokens()),
            audit_tag: None,
        }
    }

    async fn call_model(&self) -> Collected {
        let mut stream = self.model.stream(self.build_request());
        let mut c = Collected {
            text: String::new(),
            calls: vec![],
            input_tokens: 0,
            output_tokens: 0,
            // A stream that ends without a stop event did not finish.
            stop: StopReason::Error,
            message: Some("the model stream ended without a stop event".into()),
        };
        while let Some(ev) = stream.next().await {
            match ev {
                ModelEvent::Text { delta } => {
                    if let Some(cb) = &self.on_text {
                        cb(&delta);
                    }
                    c.text.push_str(&delta);
                }
                ModelEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => c.calls.push((id, name, arguments)),
                // No documents are sent, so no citations come back.
                ModelEvent::Citation { .. } => {}
                ModelEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    c.input_tokens += input_tokens;
                    c.output_tokens += output_tokens;
                }
                ModelEvent::Stop { reason, message } => {
                    c.stop = reason;
                    c.message = message;
                    break;
                }
            }
        }
        c
    }

    /// Run one turn for a user message.
    pub async fn run_turn(&mut self, user_text: &str) -> Result<TurnSummary> {
        if self.mid_turn {
            // The previous turn's future was dropped and nobody cleaned up.
            self.recover_after_cancel();
        }
        repair_history(&mut self.history, "(the previous turn was interrupted)");
        self.turn_text = user_text.to_string();
        self.turn_counter += 1;
        self.mid_turn = true;
        self.touched.clear();
        self.declarations = 0;
        // The declared scope lasts one turn (G5).
        self.policy.begin_turn();
        self.pinned.remove_prefixed("Declared scope:");
        self.pinned.remove_prefixed("Plan:");
        // The policy must know the sandbox before the first tool call (G2).
        self.policy
            .set_unsandboxed_runs(self.ctx.project.unsandboxed_runs);
        if let Some(k) = &self.ctx.kernel {
            if let Some(h) = k.hello().await {
                self.policy.set_sandbox(&h.sandbox);
            }
        }
        let mut st = TurnState::default();
        let outcome = self.turn_body(user_text, &mut st).await;
        self.finish_turn(st, outcome)
    }

    /// The one place a turn ends (A1): whatever happened, the history is left valid for both
    /// providers, the summary is recorded, and the worktree baseline is moved past the
    /// agent's own edits.
    fn finish_turn(&mut self, st: TurnState, outcome: Result<()>) -> Result<TurnSummary> {
        let TurnState {
            mut summary,
            files_changed,
            last_text,
        } = st;
        let note = match &outcome {
            Err(e) => format!("(the turn ended early: {e})"),
            Ok(()) if summary.stopped_by_max_steps => format!(
                "(stopped after the limit of {} steps; ask me to continue)",
                self.config.max_steps
            ),
            Ok(()) => "(turn ended)".to_string(),
        };
        repair_history(&mut self.history, &note);
        summary.text = last_text;
        summary.files_changed = files_changed.into_iter().collect();
        if !summary.files_changed.is_empty() {
            self.pinned.add(format!(
                "Turn {}: changed {}",
                self.turn_counter,
                summary.files_changed.join(", ")
            ));
        }
        let recorded = self.record(Event::Summary {
            ts: now_rfc3339(),
            text: summary.text.clone(),
            files_changed: summary.files_changed.clone(),
            runs: summary.runs.clone(),
            figures: summary.figures.clone(),
        });
        let _ = self.ctx.session.mark_worktree();
        self.touched.clear();
        self.mid_turn = false;
        outcome?;
        recorded?;
        Ok(summary)
    }

    async fn turn_body(&mut self, user_text: &str, st: &mut TurnState) -> Result<()> {
        if self.config.mode == Mode::Build {
            // Unique across conversations and processes; an existing ref is never replaced.
            let id = checkpoint::new_id(&self.ctx.conv_id);
            match checkpoint::create(&self.ctx.repo, &id) {
                Ok(r) => {
                    self.record(Event::Checkpoint {
                        ts: now_rfc3339(),
                        r#ref: r,
                    })?;
                    let _ = checkpoint::prune(&self.ctx.repo, self.config.checkpoint_keep);
                }
                Err(e) => tracing::warn!("checkpoint failed: {e}"),
            }
        }
        let user_changed = self
            .ctx
            .session
            .files_changed_since_last_turn()
            .unwrap_or_default();
        self.project_map = prompt::project_map(&self.ctx, &user_changed).await;

        self.record(Event::User {
            ts: now_rfc3339(),
            text: user_text.to_string(),
            attachments: None,
        })?;
        let mut text = user_text.to_string();
        if !user_changed.is_empty() {
            text.push_str(&format!(
                "\n\n[Files changed by the user since the last turn: {}]",
                user_changed.join(", ")
            ));
        }
        self.history.push(Message::user_text(text));

        let mut fix_rounds = 0u32;
        loop {
            if st.summary.steps >= self.config.max_steps {
                st.summary.stopped_by_max_steps = true;
                st.summary
                    .open_issues
                    .push(format!("stopped after max_steps={}", self.config.max_steps));
                return Ok(());
            }
            st.summary.steps += 1;
            self.maybe_compact().await;

            let c = self.call_model().await;
            st.summary.input_tokens += c.input_tokens;
            st.summary.output_tokens += c.output_tokens;
            self.record(Event::Usage {
                ts: now_rfc3339(),
                input_tokens: c.input_tokens,
                output_tokens: c.output_tokens,
                model: self.config.model_name.clone(),
            })?;
            if c.stop == StopReason::Error {
                let msg = c.message.clone().unwrap_or_else(|| "model error".into());
                // Keep whatever text arrived before the failure visible to the user.
                if !c.text.trim().is_empty() {
                    st.last_text = c.text.clone();
                }
                self.record(Event::Assistant {
                    ts: now_rfc3339(),
                    text: format!("[model error] {msg}"),
                })?;
                st.summary.open_issues.push(format!("model error: {msg}"));
                return Err(anyhow!("model error: {msg}"));
            }

            let clean_text = c.text.clone();
            let mut calls = c.calls.clone();
            // Ids key the results in the next request and in the transcript.
            for (i, call) in calls.iter_mut().enumerate() {
                if call.0.is_empty() || !self.used_call_ids.insert(call.0.clone()) {
                    call.0 = format!(
                        "tc-{}-{}-{i}-{}",
                        self.turn_counter,
                        st.summary.steps,
                        crate::util::short_rand()
                    );
                    self.used_call_ids.insert(call.0.clone());
                }
            }

            let mut assistant_parts = Vec::new();
            // Whitespace-only text is neither recorded nor sent back (A4).
            let has_text = !clean_text.trim().is_empty();
            if has_text {
                self.record(Event::Assistant {
                    ts: now_rfc3339(),
                    text: clean_text.clone(),
                })?;
                st.last_text = clean_text.clone();
            }
            if has_text {
                assistant_parts.push(ContentPart::Text {
                    text: clean_text.clone(),
                });
            }
            for (id, name, args) in &calls {
                self.record(Event::ToolCall {
                    ts: now_rfc3339(),
                    id: id.clone(),
                    name: name.clone(),
                    arguments: args.clone(),
                })?;
                assistant_parts.push(ContentPart::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: args.clone(),
                });
            }
            if assistant_parts.is_empty() {
                assistant_parts.push(ContentPart::Text {
                    text: "(no output)".into(),
                });
            }
            self.history.push(Message {
                role: Role::Assistant,
                content: assistant_parts,
            });

            if calls.is_empty() {
                if c.stop == StopReason::MaxTokens {
                    st.summary
                        .open_issues
                        .push("the model hit its output token limit".into());
                }
                return Ok(());
            }

            let mut result_parts: Vec<ContentPart> = Vec::new();
            let mut edited_this_round: Vec<String> = Vec::new();
            for (id, model_name, args) in &calls {
                st.summary.tool_calls += 1;
                let outcome = self
                    .execute_call(
                        id,
                        model_name,
                        args,
                        &mut st.summary,
                        &mut edited_this_round,
                    )
                    .await;
                for f in &edited_this_round {
                    self.touched.insert(f.clone());
                    st.files_changed.insert(f.clone());
                }
                if !outcome.ok {
                    st.summary.tool_errors += 1;
                }
                // The result goes into the history before anything that can fail, so a call
                // is never left without its result.
                result_parts.push(ContentPart::ToolResult {
                    tool_call_id: id.clone(),
                    content: serde_json::to_string(&outcome.result).unwrap_or_default(),
                    is_error: !outcome.ok,
                });
                if let Err(e) = self.record(Event::ToolResult {
                    ts: now_rfc3339(),
                    id: id.clone(),
                    ok: outcome.ok,
                    result: outcome.result.clone(),
                    truncated: if outcome.truncated { Some(true) } else { None },
                }) {
                    self.history.push(Message {
                        role: Role::User,
                        content: result_parts,
                    });
                    return Err(e);
                }
            }
            if !edited_this_round.is_empty() {
                if let Some(feedback) = self.post_edit_checks(&edited_this_round).await {
                    if fix_rounds < self.config.auto_fix_iterations {
                        fix_rounds += 1;
                        result_parts.push(ContentPart::Text {
                            text: format!("[auto-check round {fix_rounds}/{}] Diagnostics after your edits (fix errors before finishing):\n{feedback}", self.config.auto_fix_iterations),
                        });
                    } else {
                        st.summary.open_issues.push(format!(
                            "diagnostics remain after {} auto-fix rounds:\n{feedback}",
                            self.config.auto_fix_iterations
                        ));
                        result_parts.push(ContentPart::Text {
                            text: "[auto-check] Diagnostics remain but the auto-fix budget is exhausted; they will be surfaced to the user. Wrap up and report them.".into(),
                        });
                    }
                }
            }
            if c.stop == StopReason::MaxTokens {
                result_parts.push(ContentPart::Text {
                    text: "[note] Your last response was cut off at the output token limit. Keep each response and each tool call smaller.".into(),
                });
            }
            self.history.push(Message {
                role: Role::User,
                content: result_parts,
            });
        }
    }

    fn tool_error(&self, id: &str, message: impl Into<String>) -> crate::tools::ToolOutcome {
        self.ctx
            .cap_result(id, Err(crate::tools::ToolError::new(message)))
    }

    /// `declare_scope`: the first declaration of a turn is free; one that only repeats or
    /// narrows the scope is free; one that adds files goes through the approver (G5).
    async fn declare_scope(&mut self, id: &str, args: &Value) -> crate::tools::ToolOutcome {
        let files = str_array(args.get("files"));
        let change = self.policy.scope_change(&files);
        let mut widened = false;
        if let ScopeChange::Widens(added) = &change {
            let current = self
                .policy
                .scope()
                .map(<[String]>::to_vec)
                .unwrap_or_default();
            let reason = format!(
                "the agent asks to widen its declared edit scope by adding {} (scope so far: {})",
                added.join(", "),
                if current.is_empty() {
                    "nothing".to_string()
                } else {
                    current.join(", ")
                }
            );
            let req = ApprovalRequest {
                tool: "declare_scope".to_string(),
                args: json!({
                    "add": added,
                    "current_scope": current,
                    "requested_scope": files,
                    "plan": args.get("plan"),
                }),
                reason: reason.clone(),
                proposal: None,
                call_id: id.to_string(),
            };
            match self.approver.decide(&req).await {
                ApprovalDecision::Accept { .. } => widened = true,
                ApprovalDecision::Reject { reason: why } => {
                    let why = why
                        .filter(|r| !r.trim().is_empty())
                        .map(|r| format!("; user says: {r}"))
                        .unwrap_or_default();
                    return self.tool_error(
                        id,
                        format!(
                            "the user did not approve widening the scope{why}. The scope is unchanged: {}. Edits to other files still need approval one by one.",
                            current.join(", ")
                        ),
                    );
                }
            }
        }
        self.policy.declare_scope(files);
        self.declarations += 1;
        let scope = self
            .policy
            .scope()
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        self.pinned
            .set_prefixed("Declared scope: ", &scope.join(", "));
        if let Some(plan) = args.get("plan").and_then(Value::as_str) {
            self.pinned
                .set_prefixed("Plan: ", truncate_bytes(plan, 500));
        }
        // The result is the transcript's record of the scope in force from here on, with its
        // order within the turn.
        self.ctx.cap_result(
            id,
            Ok(json!({
                "ok": true,
                "scope": scope,
                "declaration": self.declarations,
                "widened_with_approval": widened,
            })),
        )
    }

    async fn execute_call(
        &mut self,
        id: &str,
        model_name: &str,
        args: &Value,
        summary: &mut TurnSummary,
        edited: &mut Vec<String>,
    ) -> crate::tools::ToolOutcome {
        let Some(name) = registry::internal_name(model_name) else {
            return self.tool_error(id, format!("unknown tool `{model_name}`"));
        };
        // Arguments the provider could not deliver as JSON are never executed (B6).
        if args.get("_truncated").and_then(Value::as_bool) == Some(true) {
            return self.tool_error(
                id,
                format!(
                    "your `{model_name}` call was cut off by the output token limit ({} tokens) before its arguments were complete, so nothing was done. Split the work: create the file with a first part and extend it with further fs_propose_edit calls, or make several smaller edits.",
                    self.config.effective_max_tokens()
                ),
            );
        }
        if args.get("_raw").is_some() && args.as_object().map(|o| o.len()) == Some(1) {
            return self.tool_error(
                id,
                format!("the arguments of your `{model_name}` call were not valid JSON, so nothing was done; send the call again with a JSON object"),
            );
        }
        match name {
            "declare_scope" => return self.declare_scope(id, args).await,
            "pin_fact" => {
                if let Some(f) = args.get("fact").and_then(Value::as_str) {
                    self.pinned.add(f);
                }
                return self.ctx.cap_result(id, Ok(json!({"ok": true})));
            }
            _ => {}
        }
        let class = registry::tool_class(name);
        let eval = self.policy.evaluate(name, args);
        let is_edit = class == Some(ToolClass::Edit);
        let mut proposal: Option<Proposal> = None;
        if is_edit && eval.decision != PolicyDecision::Deny {
            match proposals::build(&self.ctx.repo, id, name, args) {
                Ok(p) => {
                    let _ = self.record(Event::Proposal {
                        ts: now_rfc3339(),
                        id: p.id.clone(),
                        kind: p.kind,
                        summary: p.summary.clone(),
                        diff: Some(p.diff.clone()),
                    });
                    proposal = Some(p);
                }
                Err(e) => return self.ctx.cap_result(id, Err(e.into())),
            }
        }
        let pk = proposal_kind_for(name);
        let mut user_modified = false;
        match eval.decision {
            PolicyDecision::Deny => {
                if is_edit {
                    let _ = self.record(Event::Proposal {
                        ts: now_rfc3339(),
                        id: id.to_string(),
                        kind: pk,
                        summary: format!(
                            "{name} {}",
                            args.get("path").and_then(Value::as_str).unwrap_or("")
                        ),
                        diff: None,
                    });
                    let _ = self.record(Event::Decision {
                        ts: now_rfc3339(),
                        proposal_id: id.to_string(),
                        decision: Decision::Reject,
                        by: DecidedBy::Policy,
                    });
                }
                return self.tool_error(id, format!("denied by policy: {}", eval.reason));
            }
            PolicyDecision::Ask => {
                let req = ApprovalRequest {
                    tool: name.to_string(),
                    args: args.clone(),
                    reason: eval.reason.clone(),
                    proposal: proposal.clone(),
                    call_id: id.to_string(),
                };
                let decision = self.approver.decide(&req).await;
                let ok = decision.accepted();
                if !is_edit {
                    let _ = self.record(Event::Proposal {
                        ts: now_rfc3339(),
                        id: id.to_string(),
                        kind: pk,
                        summary: format!("{name} {} ({})", compact_args(args), eval.reason),
                        diff: None,
                    });
                }
                let _ = self.record(Event::Decision {
                    ts: now_rfc3339(),
                    proposal_id: id.to_string(),
                    decision: if ok {
                        Decision::Accept
                    } else {
                        Decision::Reject
                    },
                    by: DecidedBy::User,
                });
                match decision {
                    ApprovalDecision::Reject { reason } => {
                        let why = match reason {
                            Some(r) if !r.trim().is_empty() => {
                                format!("{}; user says: {r}", eval.reason)
                            }
                            _ => eval.reason.clone(),
                        };
                        return self.tool_error(id, format!("rejected by the user ({why})"));
                    }
                    ApprovalDecision::Accept { content } => {
                        if let (Some(c), Some(p)) = (content, proposal.as_mut()) {
                            if p.kind != ProposalKind::Delete {
                                p.new_content = Some(c);
                                user_modified = true;
                            }
                        }
                    }
                }
            }
            PolicyDecision::Allow => {
                if is_edit {
                    let _ = self.record(Event::Decision {
                        ts: now_rfc3339(),
                        proposal_id: id.to_string(),
                        decision: Decision::Auto,
                        by: DecidedBy::Policy,
                    });
                }
            }
        }
        if let Some(p) = proposal {
            let in_scope = self.policy.in_scope(&p.path);
            let r = proposals::apply(&self.ctx.repo, &p).map(|_| {
                edited.push(p.path.clone());
                if !in_scope {
                    summary.unwanted_edits += 1;
                }
                if user_modified {
                    json!({
                        "applied": true,
                        "path": p.path,
                        "summary": p.summary,
                        "user_modified": true,
                        "note": "the user edited your proposal before accepting it; the file on disk is their version, so re-read it before editing it again"
                    })
                } else {
                    json!({"applied": true, "path": p.path, "summary": p.summary})
                }
            });
            return self.ctx.cap_result(id, r.map_err(Into::into));
        }
        let result = self.ctx.dispatch(id, name, args).await;
        if name == "run.script" {
            if let Ok(v) = &result {
                if let Some(r) = v.get("run_id").and_then(Value::as_str) {
                    summary.runs.push(r.to_string());
                }
                if let Some(figs) = v.get("figures").and_then(Value::as_array) {
                    summary.figures.extend(
                        figs.iter()
                            .filter_map(|f| f.as_str().map(|s| s.to_string())),
                    );
                }
            }
        }
        self.ctx.cap_result(id, result)
    }

    /// Lint + contract check (including similarity) on edited files; returns feedback text when
    /// there are diagnostics. Silently skipped when the kernel is unavailable.
    async fn post_edit_checks(&self, edited: &[String]) -> Option<String> {
        let kernel = self.ctx.kernel.as_ref()?;
        let py: Vec<String> = edited
            .iter()
            .filter(|p| p.ends_with(".py"))
            .cloned()
            .collect();
        if py.is_empty() {
            return None;
        }
        let mut diags: Vec<Diagnostic> = Vec::new();
        match kernel.lint_run(Some(py.clone())).await {
            Ok(r) => diags.extend(r.diagnostics),
            Err(e) => tracing::warn!("lint.run failed: {e}"),
        }
        match kernel.contract_check(None).await {
            Ok(r) => diags.extend(
                r.diagnostics
                    .into_iter()
                    .filter(|d| py.contains(&d.path) || d.rule.starts_with("lib.")),
            ),
            Err(e) => tracing::warn!("contract.check failed: {e}"),
        }
        if diags.is_empty() {
            return None;
        }
        let mut s = String::new();
        for d in diags.iter().take(60) {
            let sev = match d.severity {
                Severity::Error => "error",
                Severity::Warn => "warn",
            };
            s.push_str(&format!(
                "- {sev} {} {}:{}:{} {}",
                d.rule, d.path, d.line, d.col, d.message
            ));
            if let Some(h) = &d.hint {
                s.push_str(&format!(" (hint: {h})"));
            }
            s.push('\n');
        }
        Some(s)
    }

    /// Tokens the request spends before any history: system prompt plus tool definitions.
    fn overhead_tokens(&self) -> usize {
        let mut n = estimate_tokens(&self.system_prompt());
        for d in registry::defs_for_mode(self.config.mode) {
            n += estimate_tokens(&d.description) + estimate_tokens(&d.parameters.to_string()) + 10;
        }
        n
    }

    #[cfg(test)]
    fn history_tokens(&self) -> usize {
        wb_transcript::compact::history_tokens(&self.history)
    }

    /// Tokens the history may use: the compaction fraction of the window minus the fixed
    /// overhead, but never less than a tenth of the window.
    fn history_budget(&self) -> usize {
        wb_transcript::staged::history_budget(
            self.config.caps.context_window,
            self.config.compaction_fraction,
            self.overhead_tokens(),
        )
    }

    /// Bring the history under budget with the shared staged compaction
    /// (`wb_transcript::staged`): shrink old tool results, then summarise the older part
    /// (never separating a tool call from its result; a mechanical digest when the summary
    /// call fails), then hard-truncate. The history is valid for both providers after
    /// every stage.
    pub async fn maybe_compact(&mut self) {
        let budget = self.history_budget();
        let summarizer = ModelSummarizer::new(self.model.clone())
            .with_max_tokens(1500.min(self.config.effective_max_tokens()));
        compact_staged(
            &mut self.history,
            budget,
            self.config.caps.context_window,
            &mut self.compact_floor,
            &summarizer,
        )
        .await;
    }
}

fn str_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn proposal_kind_for(tool: &str) -> ProposalKind {
    match tool {
        "fs.propose_create" => ProposalKind::Create,
        "fs.propose_delete" => ProposalKind::Delete,
        "pkg.add" | "pkg.remove" | "pkg.sync" => ProposalKind::Package,
        "shell.run" | "run.script" | "exec.snippet" => ProposalKind::Shell,
        "git.commit" => ProposalKind::Commit,
        _ => ProposalKind::Edit,
    }
}

fn compact_args(args: &Value) -> String {
    truncate_bytes(&args.to_string(), 200).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use analyst_protocol::config::{EditPolicy, PolicyConfig, ProjectConfig};
    use wb_providers::ScriptedModelClient;
    use wb_transcript::compact::message_tokens;
    use wb_transcript::staged::{choose_split, is_safe_boundary};

    fn setup(
        edits: EditPolicy,
        mode: Mode,
    ) -> (tempfile::TempDir, tempfile::TempDir, ToolContext, Policy) {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        checkpoint::git(repo.path(), &["init", "-q"], None).unwrap();
        std::fs::create_dir_all(repo.path().join("scripts")).unwrap();
        std::fs::write(repo.path().join("scripts/a.py"), "print('hello')\n").unwrap();
        checkpoint::git(repo.path(), &["add", "-A"], None).unwrap();
        checkpoint::git(repo.path(), &["commit", "-q", "-m", "init"], None).unwrap();
        let session = Session::create(home.path(), repo.path(), "local", "scripted/model").unwrap();
        let ctx = ToolContext::new(
            repo.path(),
            home.path(),
            session,
            "c1",
            ProjectConfig::default(),
        );
        let policy = Policy::new(
            repo.path(),
            PolicyConfig {
                edits,
                ..Default::default()
            },
            mode,
        )
        .unwrap();
        (home, repo, ctx, policy)
    }

    fn agent(
        model: Arc<ScriptedModelClient>,
        ctx: ToolContext,
        policy: Policy,
        approver: Arc<dyn Approver>,
        max_steps: u32,
    ) -> Agent {
        let mode = policy.mode();
        Agent::new(
            model,
            ctx,
            policy,
            approver,
            AgentConfig::new(
                mode,
                "scripted/model",
                ModelCapabilities::default(),
                max_steps,
            ),
        )
    }

    fn call(id: &str, name: &str, args: Value) -> ModelEvent {
        ModelEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }
    }

    fn stop_tools() -> ModelEvent {
        ModelEvent::Stop {
            reason: StopReason::ToolCalls,
            message: None,
        }
    }

    #[tokio::test]
    async fn tool_call_is_executed_and_result_appended() {
        let (_h, _r, ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                ModelEvent::Text {
                    delta: "Reading.".into(),
                },
                call("c1", "fs_read", json!({"path": "scripts/a.py"})),
                stop_tools(),
            ],
            vec![
                ModelEvent::Text {
                    delta: "Done.".into(),
                },
                ModelEvent::Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                },
            ],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 10);
        let s = a.run_turn("read a.py").await.unwrap();
        assert_eq!(s.steps, 2);
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.tool_errors, 0);
        assert_eq!(s.text, "Done.");
        assert_eq!(s.input_tokens, 5);
        let reqs = model.recorded();
        assert_eq!(reqs.len(), 2);
        let second = &reqs[1];
        assert!(second.system.as_ref().unwrap().contains("SCRIPT CONTRACT"));
        assert!(!second.tools.is_empty());
        let asst = &second.messages[1];
        assert_eq!(asst.role, Role::Assistant);
        assert!(
            matches!(&asst.content[1], ContentPart::ToolCall { name, .. } if name == "fs_read")
        );
        let res = &second.messages[2];
        assert_eq!(res.role, Role::User);
        match &res.content[0] {
            ContentPart::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, "c1");
                assert!(!is_error);
                assert!(content.contains("hello"));
            }
            other => panic!("unexpected {other:?}"),
        }
        let events = a.ctx.session.read_transcript("c1").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::ToolCall { name, .. } if name == "fs_read")));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::ToolResult { ok: true, .. })));
        assert!(matches!(events.last().unwrap(), Event::Summary { .. }));
        assert!(
            !events.iter().any(|e| matches!(e, Event::Checkpoint { .. })),
            "plan mode makes no checkpoint"
        );
    }

    #[tokio::test]
    async fn review_mode_pauses_and_rejection_fails_cleanly() {
        let (_h, repo, ctx, policy) = setup(EditPolicy::Review, Mode::Build);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call("s", "declare_scope", json!({"files": ["scripts/b.py"]})),
                call(
                    "e",
                    "fs_propose_create",
                    json!({"path": "scripts/b.py", "content": "x = 1\n"}),
                ),
                stop_tools(),
            ],
            vec![ModelEvent::Text {
                delta: "Rejected, stopping.".into(),
            }],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 10);
        let s = a.run_turn("create b.py").await.unwrap();
        assert!(!repo.path().join("scripts/b.py").exists());
        assert_eq!(s.tool_errors, 1);
        assert_eq!(s.unwanted_edits, 0);
        assert!(s.files_changed.is_empty());
        let reqs = model.recorded();
        let res = &reqs[1].messages[2];
        let err = res
            .content
            .iter()
            .find_map(|p| match p {
                ContentPart::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } if tool_call_id == "e" => Some((content.clone(), *is_error)),
                _ => None,
            })
            .unwrap();
        assert!(err.1);
        assert!(err.0.contains("rejected by the user"));
        let events = a.ctx.session.read_transcript("c1").unwrap();
        assert!(events.iter().any(|e| matches!(e, Event::Checkpoint { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Proposal {
                kind: ProposalKind::Create,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Decision {
                decision: Decision::Reject,
                by: DecidedBy::User,
                ..
            }
        )));
        assert!(!checkpoint::list(repo.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn out_of_scope_edit_under_auto_asks() {
        let (_h, repo, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call(
                    "s",
                    "declare_scope",
                    json!({"files": ["scripts/a.py"], "plan": "tweak a"}),
                ),
                stop_tools(),
            ],
            vec![
                call(
                    "in",
                    "fs_propose_edit",
                    json!({"path": "scripts/a.py", "edits": [{"old": "hello", "new": "bye"}]}),
                ),
                call(
                    "out",
                    "fs_propose_create",
                    json!({"path": "scripts/c.py", "content": "c\n"}),
                ),
                stop_tools(),
            ],
            vec![ModelEvent::Text { delta: "ok".into() }],
        ]));
        let approver = Arc::new(RecordingApprover::new(true));
        let mut a = agent(model.clone(), ctx, policy, approver.clone(), 10);
        let s = a.run_turn("edit").await.unwrap();
        let asked = approver.seen.lock().unwrap();
        assert_eq!(asked.len(), 1, "only the out-of-scope edit asks");
        assert_eq!(asked[0].tool, "fs.propose_create");
        assert!(asked[0].reason.contains("outside the declared scope"));
        assert!(asked[0].proposal.as_ref().unwrap().diff.contains("+c"));
        assert_eq!(
            std::fs::read_to_string(repo.path().join("scripts/a.py")).unwrap(),
            "print('bye')\n"
        );
        assert!(repo.path().join("scripts/c.py").is_file());
        assert_eq!(s.unwanted_edits, 1);
        assert_eq!(s.files_changed, vec!["scripts/a.py", "scripts/c.py"]);
        let events = a.ctx.session.read_transcript("c1").unwrap();
        let decisions: Vec<(Decision, DecidedBy)> = events
            .iter()
            .filter_map(|e| match e {
                Event::Decision { decision, by, .. } => Some((*decision, *by)),
                _ => None,
            })
            .collect();
        assert_eq!(
            decisions,
            vec![
                (Decision::Auto, DecidedBy::Policy),
                (Decision::Accept, DecidedBy::User)
            ]
        );
        assert!(a
            .pinned_facts()
            .iter()
            .any(|f| f.starts_with("Declared scope: scripts/a.py")));
        assert!(model.recorded()[1]
            .system
            .as_ref()
            .unwrap()
            .contains("Declared scope: scripts/a.py"));
    }

    #[tokio::test]
    async fn max_steps_bounds_the_loop() {
        let (_h, _r, ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        let scripts: Vec<Vec<ModelEvent>> = (0..20)
            .map(|i| vec![call(&format!("c{i}"), "fs_list", json!({})), stop_tools()])
            .collect();
        let model = Arc::new(ScriptedModelClient::new(scripts));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 3);
        let s = a.run_turn("loop").await.unwrap();
        assert_eq!(s.steps, 3);
        assert!(s.stopped_by_max_steps);
        assert_eq!(model.calls(), 3);
        assert!(s.open_issues.iter().any(|i| i.contains("max_steps")));
    }

    #[tokio::test]
    async fn denied_tool_returns_error_result() {
        let (_h, repo, ctx, policy) = setup(EditPolicy::Auto, Mode::Run);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call(
                    "e",
                    "fs_propose_create",
                    json!({"path": "scripts/z.py", "content": "z\n"}),
                ),
                stop_tools(),
            ],
            vec![ModelEvent::Text {
                delta: "cannot".into(),
            }],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(AutoApprover), 5);
        let s = a.run_turn("make z").await.unwrap();
        assert_eq!(s.tool_errors, 1);
        assert!(!repo.path().join("scripts/z.py").exists());
        let events = a.ctx.session.read_transcript("c1").unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Decision {
                decision: Decision::Reject,
                by: DecidedBy::Policy,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn compaction_summarizes_older_history() {
        let (_h, _r, ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        let long = "x".repeat(2000);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![ModelEvent::Text {
                delta: long.clone(),
            }],
            vec![ModelEvent::Text {
                delta: long.clone(),
            }],
            vec![ModelEvent::Text {
                delta: "SUMMARY-TEXT".into(),
            }],
            vec![ModelEvent::Text {
                delta: "final".into(),
            }],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 5);
        set_history_budget(&mut a, 800);
        a.run_turn("one").await.unwrap();
        a.run_turn("two").await.unwrap();
        a.run_turn("three").await.unwrap();
        let reqs = model.recorded();
        assert_eq!(reqs.len(), 4);
        assert!(reqs[2].messages[0]
            .text()
            .contains("Summarize this conversation segment"));
        let last = &reqs[3];
        assert!(last.messages[0]
            .text()
            .starts_with("[Summary of earlier conversation]\nSUMMARY-TEXT"));
        assert!(last.messages.iter().any(|m| m.text() == "three"));
        assert!(a.history().len() < 7);
        assert_valid(a.history());
    }

    fn text(t: &str) -> ModelEvent {
        ModelEvent::Text { delta: t.into() }
    }

    /// The shape both providers require: starts with a user message, roles alternate, every
    /// tool call is answered in the very next message, and no result lacks its call.
    fn assert_valid(history: &[Message]) {
        assert!(!history.is_empty());
        assert_eq!(history[0].role, Role::User, "{history:#?}");
        for w in history.windows(2) {
            assert_ne!(w[0].role, w[1].role, "roles must alternate: {history:#?}");
        }
        for (i, m) in history.iter().enumerate() {
            let calls: Vec<&String> = m
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ToolCall { id, .. } => Some(id),
                    _ => None,
                })
                .collect();
            let results: Vec<&String> = m
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ToolResult { tool_call_id, .. } => Some(tool_call_id),
                    _ => None,
                })
                .collect();
            if !calls.is_empty() {
                assert_eq!(m.role, Role::Assistant);
                let next: Vec<&String> = history
                    .get(i + 1)
                    .map(|n| {
                        n.content
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::ToolResult { tool_call_id, .. } => Some(tool_call_id),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(calls, next, "unanswered tool call at {i}: {history:#?}");
            }
            if !results.is_empty() {
                assert!(i > 0, "orphan result at 0");
                let prev: Vec<&String> = history[i - 1]
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::ToolCall { id, .. } => Some(id),
                        _ => None,
                    })
                    .collect();
                assert_eq!(results, prev, "orphan tool result at {i}: {history:#?}");
            }
            for p in &m.content {
                if let ContentPart::Text { text } = p {
                    assert!(!text.trim().is_empty(), "blank text at {i}");
                }
            }
        }
    }

    /// Give the history `tokens` of room on top of the fixed overhead.
    fn set_history_budget(a: &mut Agent, tokens: usize) {
        let total = a.overhead_tokens() + tokens;
        a.config.caps.context_window = (total as f32 / a.config.compaction_fraction) as u32 + 1;
    }

    /// B3: with no plain user message to cut at, the old code cut before a lone tool result.
    #[tokio::test]
    async fn compaction_never_orphans_a_tool_result() {
        let (_h, repo, ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        std::fs::write(repo.path().join("big.txt"), "y".repeat(6000)).unwrap();
        // One turn of many read calls: the only plain user message is the first one.
        let mut scripts: Vec<Vec<ModelEvent>> = (0..6)
            .map(|i| {
                vec![
                    call(&format!("r{i}"), "fs_read", json!({"path": "big.txt"})),
                    stop_tools(),
                ]
            })
            .collect();
        scripts.push(vec![text("done")]);
        // Summary calls are answered from the same queue; make them recognisable.
        let model = Arc::new(ScriptedModelClient::new(scripts));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 20);
        set_history_budget(&mut a, 2500);
        let _ = a.run_turn("read it many times").await;
        assert_valid(a.history());
        for req in model.recorded() {
            if req.tools.is_empty() {
                continue; // a summary request
            }
            assert_valid(&[req.messages.clone(), vec![Message::assistant_text("x")]].concat());
        }
        assert!(a.history_tokens() <= a.history_budget() + 50);
    }

    #[test]
    fn split_points_keep_calls_with_their_results() {
        let tc = |id: &str| Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall {
                id: id.into(),
                name: "fs_read".into(),
                arguments: json!({}),
            }],
        };
        let tr = |id: &str| Message {
            role: Role::User,
            content: vec![ContentPart::ToolResult {
                tool_call_id: id.into(),
                content: "z".repeat(400),
                is_error: false,
            }],
        };
        let h = vec![
            Message::user_text("go"),
            tc("1"),
            tr("1"),
            tc("2"),
            tr("2"),
            tc("3"),
            tr("3"),
        ];
        for i in 0..=h.len() {
            let safe = is_safe_boundary(&h, i);
            assert_eq!(safe, matches!(i, 1 | 3 | 5), "boundary {i}");
        }
        // Tight budget: the latest safe cut, never the lone result at the end.
        assert_eq!(choose_split(&h, 1), Some(5));
        assert_eq!(choose_split(&h, 300), Some(3));
        assert_eq!(choose_split(&h, 100_000), Some(1));
        assert_eq!(choose_split(&h[..1], 10), None);
    }

    /// B4: the current turn alone exceeds the budget and the summary call fails. The history
    /// must still shrink, with a bounded number of summary calls.
    #[tokio::test]
    async fn compaction_works_inside_a_turn_and_survives_summary_failure() {
        struct Flaky {
            inner: ScriptedModelClient,
            summary_calls: std::sync::atomic::AtomicU32,
        }
        impl ModelClient for Flaky {
            fn stream(&self, req: ChatRequest) -> futures::stream::BoxStream<'static, ModelEvent> {
                if req.tools.is_empty() {
                    self.summary_calls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Box::pin(futures::stream::iter(vec![ModelEvent::Stop {
                        reason: StopReason::Error,
                        message: Some("HTTP 400: context length exceeded".into()),
                    }]));
                }
                self.inner.stream(req)
            }
            fn model_id(&self) -> &str {
                "flaky"
            }
        }
        let (_h, repo, ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        std::fs::write(repo.path().join("big.txt"), "y".repeat(12_000)).unwrap();
        let steps = 16;
        let mut scripts: Vec<Vec<ModelEvent>> = (0..steps - 1)
            .map(|i| {
                vec![
                    call(&format!("r{i}"), "fs_read", json!({"path": "big.txt"})),
                    stop_tools(),
                ]
            })
            .collect();
        scripts.push(vec![text("finished")]);
        let model = Arc::new(Flaky {
            inner: ScriptedModelClient::new(scripts),
            summary_calls: Default::default(),
        });
        let mode = policy.mode();
        let mut a = Agent::new(
            model.clone(),
            ctx,
            policy,
            Arc::new(RejectAllApprover),
            AgentConfig::new(mode, "flaky/m", ModelCapabilities::default(), 40),
        );
        set_history_budget(&mut a, 2000);
        let s = a.run_turn("read the big file repeatedly").await.unwrap();
        assert_eq!(s.text, "finished");
        assert_eq!(s.steps, steps);
        assert_valid(a.history());
        let budget = a.history_budget();
        for req in model.inner.recorded() {
            let used: usize = req.messages.iter().map(message_tokens).sum();
            assert!(
                used <= budget + 50,
                "request history {used} > budget {budget}"
            );
        }
        let summaries = model
            .summary_calls
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            summaries <= steps / 2,
            "{summaries} summary calls in {steps} steps"
        );
        // The failed summaries were replaced by a digest, not by keeping everything.
        assert!(
            a.history()
                .iter()
                .any(|m| m.text().contains("automatic summary could not be produced"))
                || summaries == 0
        );
    }

    /// A1: every early exit leaves a valid history, a summary event, and a clean baseline.
    #[tokio::test]
    async fn early_exits_are_finalised() {
        // Step limit: the loop stops right after tool results.
        let (_h, _r, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        let scripts: Vec<Vec<ModelEvent>> = vec![
            vec![
                call("s", "declare_scope", json!({"files": ["scripts/n.py"]})),
                call(
                    "e",
                    "fs_propose_create",
                    json!({"path": "scripts/n.py", "content": "n = 1\n"}),
                ),
                stop_tools(),
            ],
            vec![call("l", "fs_list", json!({})), stop_tools()],
            // Next turn.
            vec![text("second turn answer")],
        ];
        let model = Arc::new(ScriptedModelClient::new(scripts));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(AutoApprover), 2);
        let s = a.run_turn("make n.py").await.unwrap();
        assert!(s.stopped_by_max_steps);
        assert_eq!(s.files_changed, vec!["scripts/n.py"]);
        assert_valid(a.history());
        assert_eq!(a.history().last().unwrap().role, Role::Assistant);
        assert!(!a.is_mid_turn());
        let events = a.ctx.session.read_transcript("c1").unwrap();
        assert!(
            matches!(events.last().unwrap(), Event::Summary { files_changed, .. } if files_changed == &vec!["scripts/n.py".to_string()])
        );
        a.run_turn("and now?").await.unwrap();
        let reqs = model.recorded();
        let last = reqs.last().unwrap();
        assert_valid(&[last.messages.clone(), vec![Message::assistant_text("x")]].concat());
        let user_text = last.messages.last().unwrap().text();
        assert!(
            !user_text.contains("Files changed by the user"),
            "the agent's own edit was reported as the user's: {user_text}"
        );

        // Model error after an edit.
        let (_h, _r, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call("s", "declare_scope", json!({"files": ["scripts/m.py"]})),
                call(
                    "e",
                    "fs_propose_create",
                    json!({"path": "scripts/m.py", "content": "m = 1\n"}),
                ),
                stop_tools(),
            ],
            vec![ModelEvent::Stop {
                reason: StopReason::Error,
                message: Some("HTTP 500".into()),
            }],
            vec![text("recovered")],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(AutoApprover), 10);
        let err = a.run_turn("make m.py").await.unwrap_err().to_string();
        assert!(err.contains("HTTP 500"));
        assert_valid(a.history());
        let events = a.ctx.session.read_transcript("c1").unwrap();
        assert!(
            matches!(events.last().unwrap(), Event::Summary { files_changed, .. } if files_changed.len() == 1),
            "a failed turn still records its summary"
        );
        let s = a.run_turn("continue").await.unwrap();
        assert_eq!(s.text, "recovered");
        let last = model.recorded().pop().unwrap();
        assert_valid(&[last.messages.clone(), vec![Message::assistant_text("x")]].concat());
        assert!(!last
            .messages
            .last()
            .unwrap()
            .text()
            .contains("Files changed by the user"));
    }

    /// A1 for cancellation: the turn future is dropped while a tool call awaits approval.
    #[tokio::test]
    async fn dropped_turn_is_repaired() {
        struct Never;
        #[async_trait::async_trait]
        impl Approver for Never {
            async fn approve(&self, _req: &ApprovalRequest) -> bool {
                std::future::pending::<bool>().await
            }
        }
        let (_h, _r, ctx, policy) = setup(EditPolicy::Review, Mode::Build);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call("l", "fs_list", json!({})),
                call(
                    "e",
                    "fs_propose_create",
                    json!({"path": "scripts/q.py", "content": "q\n"}),
                ),
                stop_tools(),
            ],
            vec![text("after cancel")],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(Never), 10);
        let dropped = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            a.run_turn("create q"),
        )
        .await;
        assert!(dropped.is_err(), "the turn should still be waiting");
        assert!(a.is_mid_turn());
        // No explicit recover call: the next turn repairs by itself.
        a.approver = Arc::new(AutoApprover);
        let s = a.run_turn("go on").await.unwrap();
        assert_eq!(s.text, "after cancel");
        let last = model.recorded().pop().unwrap();
        assert_valid(&[last.messages.clone(), vec![Message::assistant_text("x")]].concat());
        // And the explicit hook does the same thing.
        a.recover_after_cancel();
        assert_valid(a.history());
    }

    /// G5: scope is per turn, and widening it needs approval.
    #[tokio::test]
    async fn scope_resets_each_turn_and_widening_needs_approval() {
        let (_h, repo, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        let create = |id: &str, path: &str| {
            call(
                id,
                "fs_propose_create",
                json!({"path": path, "content": "x = 1\n"}),
            )
        };
        let model = Arc::new(ScriptedModelClient::new(vec![
            // Turn 1: declare a.py, then try to self-grant b.py.
            vec![
                call("s1", "declare_scope", json!({"files": ["scripts/a.py"]})),
                call(
                    "s2",
                    "declare_scope",
                    json!({"files": ["scripts/a.py", "scripts/b.py"]}),
                ),
                create("e1", "scripts/b.py"),
                // Repeating or narrowing is free.
                call("s3", "declare_scope", json!({"files": ["scripts/a.py"]})),
                stop_tools(),
            ],
            vec![text("turn one done")],
            // Turn 2: nothing declared yet, so the old scope must not apply.
            vec![
                call(
                    "e2",
                    "fs_propose_edit",
                    json!({"path": "scripts/a.py", "edits": [{"old": "hello", "new": "bye"}]}),
                ),
                stop_tools(),
            ],
            vec![text("turn two done")],
        ]));
        let approver = Arc::new(RecordingApprover::new(false));
        let mut a = agent(model.clone(), ctx, policy, approver.clone(), 10);
        let s = a.run_turn("one").await.unwrap();
        {
            let seen = approver.seen.lock().unwrap();
            assert_eq!(seen[0].tool, "declare_scope", "widening must ask");
            assert!(seen[0].reason.contains("scripts/b.py"));
            assert_eq!(seen[0].args["add"], json!(["scripts/b.py"]));
            // The widening was rejected, so b.py is still out of scope and asks too.
            assert_eq!(seen[1].tool, "fs.propose_create");
            assert!(seen[1].reason.contains("outside the declared scope"));
            assert_eq!(seen.len(), 2);
        }
        assert!(!repo.path().join("scripts/b.py").exists());
        assert_eq!(s.unwanted_edits, 0);
        assert_eq!(a.policy.scope(), Some(&["scripts/a.py".to_string()][..]));
        // Declarations and their order are in the transcript.
        let events = a.ctx.session.read_transcript("c1").unwrap();
        let decl: Vec<(bool, Value)> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolResult { id, ok, result, .. } if id.starts_with('s') => {
                    Some((*ok, result.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(decl.len(), 3);
        assert_eq!(decl[0].1["declaration"], 1);
        assert_eq!(decl[0].1["scope"], json!(["scripts/a.py"]));
        assert!(!decl[1].0, "the rejected widening is an error result");
        assert_eq!(decl[2].1["declaration"], 2);

        a.run_turn("two").await.unwrap();
        let seen = approver.seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "turn two's edit must ask: scope was reset");
        assert_eq!(seen[2].tool, "fs.propose_edit");
        assert!(seen[2].reason.contains("no scope declared"));
        assert_eq!(
            std::fs::read_to_string(repo.path().join("scripts/a.py")).unwrap(),
            "print('hello')\n"
        );
        let sys = model.recorded()[2].system.clone().unwrap();
        assert!(sys.contains("No scope declared yet"));
        assert!(!sys.contains("Declared scope: scripts/a.py"));
    }

    #[tokio::test]
    async fn approved_widening_extends_the_scope() {
        let (_h, repo, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call("s1", "declare_scope", json!({"files": ["scripts/a.py"]})),
                call(
                    "s2",
                    "declare_scope",
                    json!({"files": ["scripts/a.py", "scripts/b.py"]}),
                ),
                call(
                    "e1",
                    "fs_propose_create",
                    json!({"path": "scripts/b.py", "content": "b = 1\n"}),
                ),
                stop_tools(),
            ],
            vec![text("ok")],
        ]));
        let approver = Arc::new(RecordingApprover::new(true));
        let mut a = agent(model, ctx, policy, approver.clone(), 10);
        let s = a.run_turn("go").await.unwrap();
        assert_eq!(
            approver.seen.lock().unwrap().len(),
            1,
            "only the widening asks"
        );
        assert!(repo.path().join("scripts/b.py").is_file());
        assert_eq!(s.unwanted_edits, 0, "an in-scope edit is not unwanted");
    }

    /// G2 end to end: without a confining sandbox a run asks, and the reason says why.
    #[tokio::test]
    async fn unsandboxed_run_tools_ask() {
        let (_h, _r, ctx, policy) = setup(EditPolicy::Review, Mode::Run);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call("x", "exec_snippet", json!({"code": "open('/etc/x','w')"})),
                stop_tools(),
            ],
            vec![text("ok")],
        ]));
        let approver = Arc::new(RecordingApprover::new(false));
        let mut a = agent(model.clone(), ctx, policy, approver.clone(), 5);
        let s = a.run_turn("run it").await.unwrap();
        let seen = approver.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].tool, "exec.snippet");
        assert!(seen[0].reason.contains("unsandboxed"), "{}", seen[0].reason);
        assert_eq!(s.tool_errors, 1);
        assert!(model.recorded()[0]
            .system
            .as_ref()
            .unwrap()
            .contains("runs are unsandboxed"));
    }

    /// B6: a tool call cut off by the output limit is answered with an instruction, and
    /// nothing is written.
    #[tokio::test]
    async fn truncated_tool_call_is_not_executed() {
        let (_h, repo, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call(
                    "t",
                    "fs_propose_create",
                    json!({"_truncated": true, "_raw": "{\"path\": \"scripts/big.py\", \"content\": \"x = "}),
                ),
                ModelEvent::Stop {
                    reason: StopReason::MaxTokens,
                    message: None,
                },
            ],
            vec![text("will split")],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(AutoApprover), 5);
        let s = a.run_turn("write a big file").await.unwrap();
        assert_eq!(s.tool_errors, 1);
        assert!(!repo.path().join("scripts/big.py").exists());
        let reqs = model.recorded();
        assert_eq!(reqs[0].max_tokens, Some(16384));
        let results = &reqs[1].messages[2];
        match &results.content[0] {
            ContentPart::ToolResult {
                content, is_error, ..
            } => {
                assert!(is_error);
                assert!(content.contains("cut off by the output token limit"));
                assert!(content.contains("Split"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn max_tokens_is_configurable_and_bounded() {
        let mut c = AgentConfig::new(Mode::Build, "p/m", ModelCapabilities::default(), 5);
        assert_eq!(c.effective_max_tokens(), 16384);
        c.caps.max_output_tokens = Some(32000);
        c.caps.context_window = 200_000;
        assert_eq!(c.effective_max_tokens(), 32000);
        c.max_tokens = Some(1000);
        assert_eq!(c.effective_max_tokens(), 1000);
        c.max_tokens = None;
        c.caps.context_window = 8192;
        assert_eq!(c.effective_max_tokens(), 4096);
    }

    /// A3: a server that reuses the same call id in every response.
    #[tokio::test]
    async fn repeated_call_ids_are_made_unique_across_steps() {
        let (_h, _r, ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![call("call_1", "fs_list", json!({})), stop_tools()],
            vec![
                call("call_1", "fs_read", json!({"path": "scripts/a.py"})),
                call("call_1", "fs_list", json!({"depth": 1})),
                stop_tools(),
            ],
            vec![text("done")],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 5);
        a.run_turn("look").await.unwrap();
        let events = a.ctx.session.read_transcript("c1").unwrap();
        let mut ids: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 3);
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3, "call ids must be unique in the transcript");
        assert_valid(a.history());
    }

    /// A4: whitespace-only assistant text is neither stored nor sent.
    #[tokio::test]
    async fn whitespace_only_text_is_dropped() {
        let (_h, _r, ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                text("\n\n  "),
                call("l", "fs_list", json!({})),
                stop_tools(),
            ],
            vec![text("listed")],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 5);
        a.run_turn("list").await.unwrap();
        let asst = &model.recorded()[1].messages[1];
        assert_eq!(asst.content.len(), 1);
        assert!(matches!(asst.content[0], ContentPart::ToolCall { .. }));
        let events = a.ctx.session.read_transcript("c1").unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, Event::Assistant { text, .. } if text.trim().is_empty())));
        assert_valid(a.history());
    }

    /// B11: two conversations (or two processes) on one repo keep separate undo points.
    #[tokio::test]
    async fn checkpoints_do_not_collide_across_conversations() {
        let (home, repo, ctx, policy) = setup(EditPolicy::Review, Mode::Build);
        let m1 = Arc::new(ScriptedModelClient::new(vec![
            vec![text("a")],
            vec![text("b")],
        ]));
        let mut a1 = agent(m1, ctx, policy.clone(), Arc::new(RejectAllApprover), 3);
        a1.run_turn("one").await.unwrap();
        a1.run_turn("two").await.unwrap();
        // A second conversation in the same session starts its own counter from scratch.
        let session = Session::open(home.path(), a1.ctx.session.id()).unwrap();
        let ctx2 = ToolContext::new(
            repo.path(),
            home.path(),
            session,
            "c2",
            ProjectConfig::default(),
        );
        let m2 = Arc::new(ScriptedModelClient::new(vec![vec![text("c")]]));
        let mut a2 = agent(m2, ctx2, policy, Arc::new(RejectAllApprover), 3);
        a2.run_turn("three").await.unwrap();
        let ids: Vec<String> = checkpoint::list(repo.path())
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids.len(), 3, "{ids:?}");
        assert_eq!(ids.iter().filter(|i| i.starts_with("c1-")).count(), 2);
        assert_eq!(ids.iter().filter(|i| i.starts_with("c2-")).count(), 1);
    }

    /// Skills come from the shared loader: an always-on project skill is in the prompt in
    /// full, an Agent Skill is listed by description and loaded with `skill_load`.
    #[tokio::test]
    async fn skills_are_listed_matched_and_loaded_on_demand() {
        let (_home, repo, mut ctx, policy) = setup(EditPolicy::Review, Mode::Plan);
        let skills = repo.path().join(".analyst/skills");
        for (name, text) in [
            (
                "house-style",
                "---\nname: house-style\nalways: true\n---\nSI units on every axis.\n",
            ),
            (
                "report",
                "---\nname: report\ndescription: How to assemble the report.\n---\nUse sections.\n",
            ),
        ] {
            std::fs::create_dir_all(skills.join(name)).unwrap();
            std::fs::write(skills.join(name).join("SKILL.md"), text).unwrap();
        }
        ctx.skills = crate::skills::discover(&ctx.home, &ctx.repo);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call("k", "skill_load", json!({"name": "report"})),
                stop_tools(),
            ],
            vec![
                call("x", "skill_load", json!({"name": "nope"})),
                stop_tools(),
            ],
            vec![text("Done.")],
        ]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(RejectAllApprover), 10);
        let s = a.run_turn("write the report").await.unwrap();
        assert_eq!((s.tool_calls, s.tool_errors), (2, 1));
        let reqs = model.recorded();
        let system = reqs[0].system.as_ref().unwrap();
        assert!(system.contains("- report: How to assemble the report."));
        assert!(system.contains("ACTIVE SKILL `house-style`:\nSI units on every axis."));
        assert!(!system.contains("Use sections."));
        assert!(reqs[0].tools.iter().any(|t| t.name == "skill_load"));
        let loaded = reqs[1].messages.last().unwrap();
        assert!(matches!(
            &loaded.content[0],
            ContentPart::ToolResult { content, is_error: false, .. } if content.contains("Use sections.")
        ));
    }

    /// A transcript written before the core moved to the shared transcript crate (the
    /// fixture is a session file in the format on disk until then, including a torn line
    /// in the middle and a last line cut off by a crash) still reads and replays.
    #[tokio::test]
    async fn an_old_format_transcript_still_replays() {
        const OLD: &str = include_str!("../../tests/fixtures/transcript_v0.jsonl");
        let (_home, _repo, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        std::fs::write(ctx.session.transcript_path("c1"), OLD).unwrap();
        let events = ctx.session.read_transcript("c1").unwrap();
        // 23 whole lines; the torn one and the cut-off last one are skipped.
        assert_eq!(events.len(), 23);
        assert_eq!(ctx.session.read_events("c1").unwrap().len(), 23);
        assert!(
            matches!(&events[0], Event::User { attachments: Some(a), .. } if a == &["data/decay.csv"])
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Proposal { kind, .. } if *kind == ProposalKind::Create)));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Run { run_id, .. } if run_id == "r-0001")));
        assert!(events.iter().any(|e| matches!(e, Event::Checkpoint { .. })));
        assert!(events.iter().any(
            |e| matches!(e, Event::Summary { files_changed, .. } if files_changed == &["scripts/fit.py"])
        ));

        let model = Arc::new(ScriptedModelClient::new(vec![vec![text("continuing")]]));
        let mut a = agent(model.clone(), ctx, policy, Arc::new(AutoApprover), 10);
        a.rehydrate(&events);
        assert_valid(a.history());
        let h = a.history();
        assert!(h[0].text().starts_with("fit the decay data"));
        // Every recorded call is in the history under its model-facing name, answered.
        let calls: Vec<&str> = h
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|p| match p {
                ContentPart::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                "declare_scope",
                "pin_fact",
                "fs_propose_create",
                "run_script",
                "declare_scope",
                "fs_read"
            ]
        );
        // The call whose result was lost in the crash is answered with an error.
        assert!(h.iter().flat_map(|m| m.content.iter()).any(|p| matches!(
            p,
            ContentPart::ToolResult { tool_call_id, is_error: true, .. } if tool_call_id == "c6"
        )));
        // The agent's own failure note is not replayed as model output.
        assert!(!h.iter().any(|m| m.text().contains("[model error]")));
        assert_eq!(h.last().unwrap().role, Role::Assistant);
        // Pinned facts, the turn counter, and the unfinished turn's scope come back.
        assert!(a.pinned_facts().iter().any(|f| f == "tau is 2.5 s"));
        assert!(a
            .pinned_facts()
            .iter()
            .any(|f| f == "Turn 1: changed scripts/fit.py"));
        assert_eq!(
            a.policy.scope(),
            Some(&["scripts/residuals.py".to_string()][..])
        );
        // And the conversation continues, appending to the same file.
        a.run_turn("go on").await.unwrap();
        assert!(model.recorded()[0]
            .messages
            .iter()
            .any(|m| m.text().contains("now plot the residuals")));
        let after = a.ctx.session.read_transcript("c1").unwrap();
        assert!(after.len() > 23);
        assert!(after[23..]
            .iter()
            .any(|e| matches!(e, Event::User { text, .. } if text == "go on")));
        assert_eq!(&after[..23], &events[..], "earlier events are untouched");
    }

    /// A11: a stored transcript restores history, pinned facts, and the turn counter.
    #[tokio::test]
    async fn rehydrate_restores_history_scope_and_pins() {
        let (home, repo, ctx, policy) = setup(EditPolicy::Auto, Mode::Build);
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                text("Planning."),
                call(
                    "s",
                    "declare_scope",
                    json!({"files": ["scripts/a.py"], "plan": "tweak a"}),
                ),
                call("p", "pin_fact", json!({"fact": "tau is 2.5"})),
                call(
                    "e",
                    "fs_propose_edit",
                    json!({"path": "scripts/a.py", "edits": [{"old": "hello", "new": "bye"}]}),
                ),
                stop_tools(),
            ],
            vec![text("Edited a.py.")],
        ]));
        let mut a = agent(model, ctx, policy.clone(), Arc::new(AutoApprover), 10);
        a.run_turn("change the greeting").await.unwrap();
        let original = a.history().to_vec();
        let events = a.ctx.session.read_transcript("c1").unwrap();

        let session = Session::open(home.path(), a.ctx.session.id()).unwrap();
        let ctx2 = ToolContext::new(
            repo.path(),
            home.path(),
            session,
            "c1",
            ProjectConfig::default(),
        );
        let model2 = Arc::new(ScriptedModelClient::new(vec![vec![text("continuing")]]));
        let mut b = agent(model2.clone(), ctx2, policy, Arc::new(AutoApprover), 10);
        b.rehydrate(&events);
        assert_valid(b.history());
        assert_eq!(b.history().len(), original.len());
        assert_eq!(b.history()[0].text(), "change the greeting");
        assert!(b.history()[1]
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::ToolCall { name, .. } if name == "fs_propose_edit")));
        assert_eq!(b.history().last().unwrap().text(), "Edited a.py.");
        assert!(b.pinned_facts().iter().any(|f| f == "tau is 2.5"));
        assert!(b
            .pinned_facts()
            .iter()
            .any(|f| f == "Turn 1: changed scripts/a.py"));
        assert!(
            b.policy.scope().is_none(),
            "a finished turn's scope is gone"
        );
        b.run_turn("what did you do?").await.unwrap();
        let req = &model2.recorded()[0];
        assert_eq!(req.messages.len(), original.len() + 1);
        assert!(req.system.as_ref().unwrap().contains("tau is 2.5"));

        // A transcript that stops mid-turn keeps that turn's scope and is repaired.
        let cut = events
            .iter()
            .position(|e| matches!(e, Event::ToolResult { id, .. } if id == "p"))
            .unwrap();
        b.rehydrate(&events[..=cut]);
        assert_valid(b.history());
        assert_eq!(b.policy.scope(), Some(&["scripts/a.py".to_string()][..]));
        assert!(b
            .pinned_facts()
            .iter()
            .any(|f| f.starts_with("Plan: tweak a")));
    }

    #[test]
    fn repair_fixes_every_malformation() {
        let mut h = vec![
            Message {
                role: Role::User,
                content: vec![ContentPart::ToolResult {
                    tool_call_id: "ghost".into(),
                    content: "orphan".into(),
                    is_error: false,
                }],
            },
            Message::user_text("first"),
            Message::user_text("second"),
            Message::assistant_text("   "),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentPart::ToolCall {
                        id: "a".into(),
                        name: "fs_read".into(),
                        arguments: json!({}),
                    },
                    ContentPart::ToolCall {
                        id: "b".into(),
                        name: "fs_list".into(),
                        arguments: json!({}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentPart::Text {
                        text: "note".into(),
                    },
                    ContentPart::ToolResult {
                        tool_call_id: "b".into(),
                        content: "ok".into(),
                        is_error: false,
                    },
                ],
            },
        ];
        repair_history(&mut h, "(closed)");
        assert_valid(&h);
        assert_eq!(h.len(), 4);
        assert_eq!(h[0].text(), "first\nsecond");
        match &h[2].content[0] {
            ContentPart::ToolResult {
                tool_call_id,
                is_error,
                ..
            } => {
                assert_eq!(tool_call_id, "a");
                assert!(is_error);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(h[3].text(), "(closed)");
        let mut empty: Vec<Message> = vec![];
        repair_history(&mut empty, "x");
        assert!(empty.is_empty());
    }
}
