//! The agent loop (ARCHITECTURE 7): outline, build, polish, with proposals gated by policy.
//!
//! The turn structure (model call, tool calls in order, pause on ask, verification feedback,
//! bounded steps, summary) is ported from Personal Analyst `crates/core/src/agent/mod.rs` at
//! commit f229e4d. There are no model-specific branches: behavior adapts only through the
//! probed capability record.

pub mod dispatch;
pub mod tools;

use crate::audit::AuditLog;
use crate::workspace::Workspace;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use dispatch::{Change, Prepared, ToolOutcome, ToolState};
use futures::StreamExt;
use present_protocol::config::SnapshotReason;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;
use tools::Mode;
use wb_policy::{Action, Decision, Policy, Reason, Scope};
use wb_providers::model::{
    ChatRequest, ContentPart, Message, ModelCapabilities, ModelEvent, Role, StopReason, ToolSupport,
};
use wb_providers::{text_protocol, ModelClient};
use wb_transcript::{
    CompactionConfig, ContextEstimate, DecidedBy, Event, ModelSummarizer, ProposalDecision,
    ResultCap, TranscriptStore,
};

pub const SYSTEM_PROMPT: &str = include_str!("prompt.md");

/// A request for the user's decision.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub id: String,
    pub tool: String,
    pub kind: String,
    pub summary: String,
    pub diff: String,
    /// Why the user is being asked: `policy`, `out_of_scope`, `always_ask`.
    pub reason: String,
    pub change: Option<Change>,
}

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Accept,
    /// The user edited the proposal before accepting it.
    AcceptModified(Box<Change>),
    Reject {
        note: Option<String>,
    },
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision;
}

/// Accepts everything (`--auto`, evaluations).
pub struct AutoApprover;

#[async_trait]
impl Approver for AutoApprover {
    async fn decide(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Accept
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub mode: Mode,
    /// `provider/model`, for the transcript and the audit log.
    pub model_name: String,
    pub provider: String,
    pub provider_location: wb_providers::config::Location,
    pub base_url_host: String,
    pub caps: ModelCapabilities,
    pub max_steps: u32,
    pub auto_fix_iterations: u32,
    pub max_tokens: Option<u32>,
    pub compaction_fraction: f32,
}

impl AgentConfig {
    pub fn new(
        mode: Mode,
        model_name: &str,
        caps: ModelCapabilities,
        max_steps: u32,
    ) -> AgentConfig {
        AgentConfig {
            mode,
            model_name: model_name.to_string(),
            provider: model_name.split('/').next().unwrap_or("").to_string(),
            provider_location: wb_providers::config::Location::External,
            base_url_host: String::new(),
            caps,
            max_steps,
            auto_fix_iterations: 3,
            max_tokens: Some(16000),
            compaction_fraction: 0.6,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TurnSummary {
    pub turn_id: String,
    pub text: String,
    pub steps: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub slides_changed: Vec<String>,
    pub exports: Vec<String>,
    pub open_issues: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub stopped_by_max_steps: bool,
    /// Proposals that needed approval because they were outside the declared scope.
    pub out_of_scope: u32,
    pub snapshot_id: Option<String>,
}

pub type TextSink = Arc<dyn Fn(&str) + Send + Sync>;
pub type EventSink = Arc<dyn Fn(&Event) + Send + Sync>;

pub struct Agent {
    pub model: Arc<dyn ModelClient>,
    pub ws: Arc<Workspace>,
    pub policy: Policy,
    pub approver: Arc<dyn Approver>,
    pub config: AgentConfig,
    pub store: TranscriptStore,
    pub conv_id: String,
    pub state: ToolState,
    pub audit: Option<Arc<AuditLog>>,
    pub on_text: Option<TextSink>,
    pub on_event: Option<EventSink>,
    history: Vec<Message>,
    scope: Scope,
    new_slides_left: u32,
    turn_counter: u32,
    project_map: String,
    proposal_counter: u32,
}

struct Collected {
    text: String,
    calls: Vec<(String, String, Value)>,
    input_tokens: u64,
    output_tokens: u64,
    stop: StopReason,
    message: Option<String>,
}

fn reason_str(reason: Reason) -> &'static str {
    match reason {
        Reason::Policy => "policy",
        Reason::OutOfScope => "out_of_scope",
        Reason::Locked => "locked",
        Reason::AlwaysAsk => "always_ask",
        Reason::Mode => "mode",
    }
}

fn mode_instructions(mode: Mode, code_mode: bool) -> String {
    let base = match mode {
        Mode::Outline => "MODE outline: work on the storyline only. Read the brief and the references, then call outline_propose with one row per slide: an action title, the message it proves, the layout you intend, and the citations that support it. You cannot create or change slides in this mode.",
        Mode::Build => "MODE build: create and change slides. Before your first change on a task, call scope_declare with the slides you will change, how many you will add, and a one-paragraph plan. Follow the approved outline when there is one. After each slide, look at it with slide_render and fix what the checker reports.",
        Mode::Polish => "MODE polish: improve wording, layout choice, and order of existing slides. You cannot add slides, delete slides, or add new claims or citations; if the user asks for new content, tell them to switch to build mode.",
    };
    if code_mode {
        format!("{base}\nThis project is in CODE GENERATION mode: the deck is the Python script deck/build.py, built with the deck SDK (`from present_sdk import Deck, cite, theme`). Read it with code_read, change it with code_propose_edit, and run it with deck_build. Use the SDK for layouts, placeholders, theme colors, charts, and diagrams so the deck stays on the template; use `slide.raw` (python-pptx) only for what the SDK cannot express. Attach citations with cite(obj, \"ref:...\").")
    } else {
        base.to_string()
    }
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: Arc<dyn ModelClient>,
        ws: Arc<Workspace>,
        policy: Policy,
        approver: Arc<dyn Approver>,
        config: AgentConfig,
        store: TranscriptStore,
        conv_id: &str,
    ) -> Agent {
        Agent {
            model,
            ws,
            policy,
            approver,
            config,
            store,
            conv_id: conv_id.to_string(),
            state: ToolState::default(),
            audit: None,
            on_text: None,
            on_event: None,
            history: Vec::new(),
            scope: Scope::undeclared(),
            new_slides_left: 0,
            turn_counter: 0,
            project_map: String::new(),
            proposal_counter: 0,
        }
    }

    /// Continue an existing conversation: rebuild the model history from its transcript.
    pub fn resume(&mut self) -> Result<()> {
        if !self.store.exists(&self.conv_id) {
            return Ok(());
        }
        let events = self.store.read(&self.conv_id)?;
        let options = wb_transcript::ReplayOptions {
            text_protocol: self.uses_text_protocol(),
            ..Default::default()
        };
        self.history = wb_transcript::to_messages(&events, &options);
        self.turn_counter = events.iter().filter(|e| e.type_name() == "user").count() as u32;
        if let Ok(pinned) = self.store.load_pinned(&self.conv_id) {
            self.state.pinned = pinned.facts().to_vec();
        }
        Ok(())
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.config.mode = mode;
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    fn uses_text_protocol(&self) -> bool {
        self.config.caps.tools != ToolSupport::Native
    }

    fn code_mode(&self) -> bool {
        self.ws
            .deck()
            .read_deck()
            .map(|d| d.mode == present_protocol::deck::DeckMode::Code)
            .unwrap_or(false)
    }

    fn record(&self, event: Event) -> Result<()> {
        if let Some(cb) = &self.on_event {
            cb(&event);
        }
        self.store.append(&self.conv_id, &event)?;
        Ok(())
    }

    fn policy_summary(&self) -> String {
        let level = |class: &str| self.policy.level(class).as_str();
        format!(
            "slide and code changes are {}; running code is {}; skill scripts are {}; exports are {}. Locked slides can never be changed.",
            match self.policy.edits {
                wb_policy::EditGate::Review => "reviewed by the user one at a time",
                wb_policy::EditGate::AutoNew => "applied directly for new slides and reviewed for changes to existing ones",
                wb_policy::EditGate::Auto => "applied directly when inside the declared scope",
            },
            level(tools::CLASS_CODE),
            level(tools::CLASS_SKILLS),
            level(tools::CLASS_EXPORT),
        )
    }

    fn system_prompt(&self) -> String {
        let code_mode = self.code_mode();
        let mut s = String::from(SYSTEM_PROMPT);
        s.push_str("\n\n");
        s.push_str(&mode_instructions(self.config.mode, code_mode));
        s.push_str("\n\nPOLICY: ");
        s.push_str(&self.policy_summary());
        if let Some(skills) = &self.state.skills {
            let enabled: Vec<String> = skills
                .list()
                .into_iter()
                .map(|i| i.name)
                .filter(|n| !self.state.disabled_skills.contains(n))
                .collect();
            let block = skills.system_prompt_block(&enabled);
            if !block.trim().is_empty() {
                s.push_str("\n\n");
                s.push_str(&block);
            }
        }
        s.push_str("\n\n");
        s.push_str(&self.project_map);
        if !self.state.pinned.is_empty() {
            s.push_str("\nPINNED FACTS:\n");
            for fact in &self.state.pinned {
                s.push_str("- ");
                s.push_str(fact);
                s.push('\n');
            }
        }
        if self.uses_text_protocol() {
            s.push_str("\n\n");
            s.push_str(&text_protocol::instructions(&tools::defs_for(
                self.config.mode,
                code_mode,
            )));
        }
        s
    }

    /// The project map (ARCHITECTURE 7.1 step 2): brief, outline, layouts, references.
    async fn build_project_map(&mut self, user_changed: &[String]) -> String {
        let ws = self.ws.clone();
        let mut s = String::from("PROJECT MAP\n");
        let mode = self.config.mode;
        for (label, tool) in [
            ("Deck", "deck.outline"),
            ("Layouts", "template.layouts"),
            ("References", "refs.list"),
        ] {
            if let Prepared::Done(out) =
                dispatch::prepare(&ws, &mut self.state, mode, tool, &json!({})).await
            {
                if out.ok {
                    let mut text = serde_json::to_string(&out.result).unwrap_or_default();
                    if text.len() > 12_000 {
                        let cut = (0..=12_000)
                            .rev()
                            .find(|i| text.is_char_boundary(*i))
                            .unwrap_or(0);
                        text.truncate(cut);
                        text.push_str("… (truncated; call the tool for the rest)");
                    }
                    s.push_str(&format!("{label}: {text}\n"));
                }
            }
        }
        if !user_changed.is_empty() {
            s.push_str(&format!(
                "Slides changed by the user since your last turn: {}\n",
                user_changed.join(", ")
            ));
        }
        s
    }

    fn build_request(&self) -> ChatRequest {
        ChatRequest {
            model: self.model.model_id().to_string(),
            system: Some(self.system_prompt()),
            messages: self.history.clone(),
            tools: if self.uses_text_protocol() {
                Vec::new()
            } else {
                tools::defs_for(self.config.mode, self.code_mode())
            },
            max_tokens: self.config.max_tokens,
            temperature: None,
            audit_tag: Some(format!("{}:{}", self.conv_id, self.turn_counter)),
        }
    }

    async fn call_model(&self) -> Collected {
        let mut stream = self.model.stream(self.build_request());
        let mut c = Collected {
            text: String::new(),
            calls: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            stop: StopReason::End,
            message: None,
        };
        while let Some(event) = stream.next().await {
            match event {
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
                _ => {}
            }
        }
        c
    }

    fn audit(&self, turn_id: &str, c: &Collected, excerpts: &[(String, u64)], images: &[String]) {
        let Some(log) = &self.audit else { return };
        let record = present_protocol::audit::AuditRecord {
            ts: crate::util::now_iso(),
            project: self
                .ws
                .deck()
                .read_deck()
                .map(|d| d.title)
                .unwrap_or_default(),
            conversation: self.conv_id.clone(),
            turn: turn_id.to_string(),
            provider: self.config.provider.clone(),
            location: match self.config.provider_location {
                wb_providers::config::Location::Internal => {
                    present_protocol::config::Location::Internal
                }
                wb_providers::config::Location::External => {
                    present_protocol::config::Location::External
                }
            },
            base_url_host: self.config.base_url_host.clone(),
            model: self.model.model_id().to_string(),
            input_tokens: c.input_tokens,
            output_tokens: c.output_tokens,
            refs: excerpts
                .iter()
                .filter_map(|(cite, chars)| {
                    cite.parse()
                        .ok()
                        .map(|cite| present_protocol::audit::AuditRef {
                            cite,
                            chars: *chars,
                        })
                })
                .collect(),
            images: images
                .iter()
                .map(|id| present_protocol::audit::AuditImage {
                    source: present_protocol::audit::AuditImageSource::Slide,
                    id: id.clone(),
                })
                .collect(),
            status: if c.stop == StopReason::Error {
                present_protocol::audit::AuditStatus::Error
            } else {
                present_protocol::audit::AuditStatus::Ok
            },
            error: if c.stop == StopReason::Error {
                Some("model_error".into())
            } else {
                None
            },
        };
        if let Err(e) = log.append(&record) {
            tracing::warn!("audit log write failed: {e}");
        }
    }

    /// Slides whose files differ from the head snapshot: the user's edits since the last turn.
    fn user_changes(&self) -> Vec<String> {
        let snaps = self.ws.snapshots();
        let Ok(Some(head)) = snaps.head() else {
            return Vec::new();
        };
        let Ok(manifest) = snaps.manifest(&head) else {
            return Vec::new();
        };
        let store = self.ws.deck();
        let mut changed = Vec::new();
        let known: std::collections::BTreeMap<&str, &str> = manifest
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.sha256.as_str()))
            .collect();
        if let Ok(deck) = store.read_deck() {
            for id in deck.slide_ids() {
                let path = format!("deck/slides/{id}.json");
                let now = store.slide_hash(id).ok();
                if known.get(path.as_str()).copied() != now.as_deref() {
                    changed.push(id.to_string());
                }
            }
        }
        changed
    }

    /// Run one turn for a user message.
    pub async fn run_turn(&mut self, user_text: &str) -> Result<TurnSummary> {
        self.turn_counter += 1;
        let turn_id = format!("{}-{}", self.conv_id, self.turn_counter);
        let mut summary = TurnSummary {
            turn_id: turn_id.clone(),
            ..Default::default()
        };
        let user_changed = self.user_changes();
        if self.config.mode != Mode::Outline {
            match self.ws.snapshot(SnapshotReason::Turn, Some(&turn_id), None) {
                Ok(id) => {
                    self.record(Event::snapshot(id.to_string(), "turn"))?;
                    summary.snapshot_id = Some(id.to_string());
                }
                Err(e) => tracing::warn!("snapshot failed: {e}"),
            }
        }
        self.scope = Scope::undeclared();
        self.new_slides_left = 0;
        self.project_map = self.build_project_map(&user_changed).await;
        self.record(Event::user(user_text))?;
        wb_transcript::ensure_ready_for_user(&mut self.history);
        self.history.push(Message::user_text(user_text));

        let cap = ResultCap::new(self.store.root().join("artifacts"))
            .with_hint("narrow the request (one slide, one passage, a smaller range)");
        let mut fix_rounds = 0u32;
        let mut changed: BTreeSet<String> = BTreeSet::new();
        let mut viewed: BTreeSet<String> = BTreeSet::new();
        let mut review_prompted = false;
        let mut last_text = String::new();
        let mut pending_excerpts: Vec<(String, u64)> = Vec::new();
        let mut pending_images: Vec<String> = Vec::new();
        loop {
            if summary.steps >= self.config.max_steps {
                summary.stopped_by_max_steps = true;
                summary
                    .open_issues
                    .push(format!("stopped after max_steps={}", self.config.max_steps));
                break;
            }
            summary.steps += 1;
            self.maybe_compact().await;

            let c = self.call_model().await;
            self.audit(&turn_id, &c, &pending_excerpts, &pending_images);
            pending_excerpts.clear();
            pending_images.clear();
            summary.input_tokens += c.input_tokens;
            summary.output_tokens += c.output_tokens;
            self.record(Event::usage(
                c.input_tokens,
                c.output_tokens,
                self.config.model_name.clone(),
            ))?;
            if c.stop == StopReason::Error {
                let msg = c.message.clone().unwrap_or_else(|| "model error".into());
                self.record(Event::assistant(format!("[model error] {msg}")))?;
                self.history
                    .push(Message::assistant_text(format!("[model error] {msg}")));
                return Err(anyhow!("model error: {msg}"));
            }

            let (clean_text, mut calls) = if self.uses_text_protocol() {
                let (text, parsed) = text_protocol::parse_tool_calls(&c.text);
                let calls = parsed
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| {
                        (
                            format!("tc-{}-{}-{i}", self.turn_counter, summary.steps),
                            p.name,
                            p.arguments,
                        )
                    })
                    .collect::<Vec<_>>();
                (text, calls)
            } else {
                (c.text.clone(), Vec::new())
            };
            calls.extend(c.calls.clone());

            let mut assistant_parts = Vec::new();
            if !clean_text.trim().is_empty() {
                self.record(Event::assistant(clean_text.clone()))?;
                last_text = clean_text.clone();
            }
            if self.uses_text_protocol() {
                assistant_parts.push(ContentPart::Text {
                    text: c.text.clone(),
                });
            } else if !clean_text.is_empty() {
                assistant_parts.push(ContentPart::Text {
                    text: clean_text.clone(),
                });
            }
            for (id, name, args) in &calls {
                let internal = tools::internal_name(name).unwrap_or(name.as_str());
                self.record(Event::tool_call(id.clone(), internal, args.clone()))?;
                if !self.uses_text_protocol() {
                    assistant_parts.push(ContentPart::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: args.clone(),
                    });
                }
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
                    summary
                        .open_issues
                        .push("the model hit its output token limit".into());
                }
                // Self-review (ARCHITECTURE 7.4): every new or changed slide is looked at once.
                let unseen: Vec<String> = changed
                    .iter()
                    .filter(|s| !viewed.contains(*s) && self.ws.deck().slide_path_exists(s))
                    .cloned()
                    .collect();
                if !unseen.is_empty()
                    && !review_prompted
                    && self.config.caps.vision
                    && !self.state.render_unavailable
                    && !self.code_mode()
                {
                    review_prompted = true;
                    self.history.push(Message::user_text(format!("[self-review] You changed {} but have not looked at {}. Call slide_render on each, fix anything that looks wrong, then finish.", if unseen.len() == 1 { "a slide" } else { "slides" }, unseen.join(", "))));
                    continue;
                }
                break;
            }

            let mut result_parts: Vec<ContentPart> = Vec::new();
            let mut changed_this_round: Vec<String> = Vec::new();
            for (id, model_name, args) in &calls {
                summary.tool_calls += 1;
                let outcome = self
                    .execute_call(id, model_name, args, &mut summary, &mut changed_this_round)
                    .await;
                let capped = cap.cap(&format!("{}/{id}", self.conv_id), outcome.result.clone());
                self.record(Event::tool_result(
                    id.clone(),
                    outcome.ok,
                    capped.value.clone(),
                    capped.truncated,
                ))?;
                if !outcome.ok {
                    summary.tool_errors += 1;
                }
                pending_excerpts.extend(outcome.excerpts.iter().cloned());
                pending_images.extend(outcome.viewed.iter().cloned());
                viewed.extend(outcome.viewed.iter().cloned());
                let content = serde_json::to_string(&capped.value).unwrap_or_default();
                if self.uses_text_protocol() {
                    result_parts.push(ContentPart::Text {
                        text: text_protocol::format_result(model_name, &capped.value),
                    });
                } else {
                    result_parts.push(ContentPart::ToolResult {
                        tool_call_id: id.clone(),
                        content,
                        is_error: !outcome.ok,
                    });
                }
                if self.config.caps.vision {
                    for (media_type, data) in outcome.images {
                        result_parts.push(ContentPart::Image { media_type, data });
                    }
                }
            }
            if !changed_this_round.is_empty() {
                for id in &changed_this_round {
                    changed.insert(id.clone());
                    viewed.remove(id);
                }
                if let Some(feedback) = self.post_edit_checks(&changed_this_round).await {
                    if fix_rounds < self.config.auto_fix_iterations {
                        fix_rounds += 1;
                        result_parts.push(ContentPart::Text { text: format!("[auto-check round {fix_rounds}/{}] The deck checker found problems after your changes (fix errors before finishing):\n{feedback}", self.config.auto_fix_iterations) });
                    } else {
                        summary.open_issues.push(format!(
                            "checker findings remain after {} fix rounds:\n{feedback}",
                            self.config.auto_fix_iterations
                        ));
                        result_parts.push(ContentPart::Text { text: "[auto-check] Problems remain but the fix budget is used up; they will be shown to the user. Wrap up and report them.".into() });
                    }
                }
            }
            self.history.push(Message {
                role: Role::User,
                content: result_parts,
            });
        }

        summary.text = last_text;
        summary.slides_changed = changed.into_iter().collect();
        if !summary.slides_changed.is_empty() {
            let note = format!(
                "Turn {}: changed {}",
                self.turn_counter,
                summary.slides_changed.join(", ")
            );
            self.state.pinned.push(note);
        }
        let mut pinned = wb_transcript::PinnedFacts::new();
        for fact in &self.state.pinned {
            pinned.add(fact);
        }
        let _ = self.store.save_pinned(&self.conv_id, &pinned);
        self.record(Event::summary(summary.text.clone(), json!({"slides_changed": summary.slides_changed, "exports": summary.exports, "open_issues": summary.open_issues})))?;
        Ok(summary)
    }

    async fn maybe_compact(&mut self) {
        let cfg = CompactionConfig::new(self.config.caps.context_window)
            .with_fraction(self.config.compaction_fraction);
        let est = ContextEstimate::heuristic(&self.system_prompt());
        if !wb_transcript::needs_compaction(&self.history, &cfg, &est) {
            return;
        }
        let summarizer = ModelSummarizer::new(self.model.clone());
        let outcome =
            wb_transcript::maybe_compact(&mut self.history, &cfg, &est, &summarizer).await;
        if let Some(event) = outcome.event() {
            let _ = self.record(event);
        }
    }

    /// Checker findings (errors only) for the slides changed in this round, as text for the model.
    async fn post_edit_checks(&mut self, changed: &[String]) -> Option<String> {
        let ws = self.ws.clone();
        let existing: Vec<String> = changed
            .iter()
            .filter(|s| ws.deck().slide_path_exists(s))
            .cloned()
            .collect();
        let scope = if self.code_mode() {
            None
        } else {
            Some(existing.as_slice())
        };
        if scope.is_some_and(|s| s.is_empty()) {
            return None;
        }
        match dispatch::check_slides(&ws, &mut self.state, scope).await {
            Ok(diags) => {
                let _ = self.record(Event::other("check", json!({"diagnostics": diags.len(), "errors": diags.iter().filter(|d| d["severity"] == "error").count(), "slides": changed})));
                let errors: Vec<String> = diags
                    .iter()
                    .filter(|d| d["severity"] == "error")
                    .take(20)
                    .map(|d| {
                        let place = [d["slide_id"].as_str(), d["element_id"].as_str()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" ");
                        let hint = d["fix_hint"]
                            .as_str()
                            .map(|h| format!(" ({h})"))
                            .unwrap_or_default();
                        format!(
                            "- [{}] {place}: {}{hint}",
                            d["rule"].as_str().unwrap_or(""),
                            d["message"].as_str().unwrap_or("")
                        )
                    })
                    .collect();
                if errors.is_empty() {
                    None
                } else {
                    Some(errors.join("\n"))
                }
            }
            Err(e) => Some(format!("- [build] the deck could not be built: {e}")),
        }
    }

    fn next_proposal_id(&mut self) -> String {
        self.proposal_counter += 1;
        format!("p-{}-{}", self.turn_counter, self.proposal_counter)
    }

    async fn execute_call(
        &mut self,
        call_id: &str,
        model_name: &str,
        args: &Value,
        summary: &mut TurnSummary,
        changed: &mut Vec<String>,
    ) -> ToolOutcome {
        let Some(name) = tools::internal_name(model_name) else {
            return ToolOutcome::err(format!("unknown tool {model_name}"));
        };
        let Some(spec) = tools::find(name) else {
            return ToolOutcome::err(format!("unknown tool {model_name}"));
        };
        if !spec.modes.contains(&self.config.mode) {
            return ToolOutcome::err(format!(
                "{model_name} is not available in {} mode",
                self.config.mode.as_str()
            ));
        }
        let ws = self.ws.clone();
        if name == "scope.declare" {
            let ids: Vec<String> = args["slide_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            let new_slides = args["new_slides"].as_u64().unwrap_or(0) as u32;
            self.scope.declare(ids.clone(), new_slides > 0);
            self.new_slides_left = new_slides;
            return ToolOutcome::ok(json!({"declared": ids, "new_slides": new_slides}));
        }
        // Tools that act directly are gated by their class before they run.
        if spec.class != tools::CLASS_READ
            && spec.class != tools::CLASS_META
            && spec.class != tools::CLASS_EDIT
        {
            match self
                .policy
                .evaluate(&Action::new(name, spec.class), &self.scope)
            {
                Decision::Deny { .. } => {
                    return ToolOutcome::err(format!(
                        "{model_name} is switched off by the project's policy"
                    ))
                }
                Decision::Ask { reason } => {
                    let id = self.next_proposal_id();
                    let kind = match name {
                        "deck.build" => "build",
                        "skill.run_script" => "skill-script",
                        "export.run" => "export",
                        _ => "action",
                    };
                    let request = ApprovalRequest {
                        id: id.clone(),
                        tool: name.into(),
                        kind: kind.into(),
                        summary: format!("{model_name} {}", compact_args(args)),
                        diff: String::new(),
                        reason: reason_str(reason).into(),
                        change: None,
                    };
                    let _ = self.record(Event::proposal(
                        id.clone(),
                        kind,
                        request.summary.clone(),
                        None,
                        None,
                    ));
                    match self.approver.decide(&request).await {
                        ApprovalDecision::Reject { note } => {
                            let _ = self.record(Event::decision(
                                id,
                                ProposalDecision::Reject,
                                DecidedBy::User,
                            ));
                            return ToolOutcome::err(format!(
                                "the user declined{}",
                                note.map(|n| format!(": {n}")).unwrap_or_default()
                            ));
                        }
                        _ => {
                            let _ = self.record(Event::decision(
                                id,
                                ProposalDecision::Accept,
                                DecidedBy::User,
                            ));
                        }
                    }
                }
                Decision::Allow => {}
            }
        }
        let prepared = dispatch::prepare(&ws, &mut self.state, self.config.mode, name, args).await;
        let change = match prepared {
            Prepared::Done(outcome) => {
                if name == "export.run" && outcome.ok {
                    if let Some(path) = outcome.result["path"].as_str() {
                        summary.exports.push(path.to_string());
                    }
                }
                if name == "deck.build" && outcome.ok {
                    changed.push("deck/build.py".into());
                }
                return outcome;
            }
            Prepared::Denied { reason, message } => {
                let id = self.next_proposal_id();
                let _ = self.record(Event::proposal(
                    id.clone(),
                    "denied",
                    message.clone(),
                    None,
                    None,
                ));
                let _ = self.record(Event::decision(
                    id,
                    ProposalDecision::Reject,
                    DecidedBy::Policy,
                ));
                return ToolOutcome {
                    ok: false,
                    result: json!({"error": message, "denied": reason}),
                    ..Default::default()
                };
            }
            Prepared::Change(change) => change,
        };

        // A change: policy first, then the user when policy says ask.
        let creates = change.creates_new();
        let mut action = Action::edit(name, change.targets())
            .creates_new(creates)
            .always_ask(matches!(change, Change::ModeSwitch { .. }));
        if matches!(change, Change::Outline { .. }) {
            // The storyline is always the user's call unless edits are fully automatic.
            action = action.creates_new(true);
        }
        let mut scope = self.scope.clone();
        if matches!(change, Change::SlideCreate { .. }) && self.new_slides_left == 0 {
            scope.allow_new = false;
        }
        if matches!(change, Change::Outline { .. } | Change::CodeEdit { .. })
            && !scope.is_declared()
        {
            // Outlines and the build script are single artifacts; declaring them adds nothing.
            scope.declare(change.targets(), true);
        }
        let decision = self.policy.evaluate(&action, &scope);
        let id = self.next_proposal_id();
        let slide_ids = change.slide_ids();
        let _ = self.record(Event::proposal(
            id.clone(),
            change.kind(),
            change.summary(),
            Some(change.diff()),
            Some(slide_ids.clone()),
        ));
        let (accepted, by) = match decision {
            Decision::Deny { reason } => {
                let _ = self.record(Event::decision(
                    id,
                    ProposalDecision::Reject,
                    DecidedBy::Policy,
                ));
                let why = if reason == Reason::Locked {
                    "it is locked by the user"
                } else {
                    "the project's policy does not allow it"
                };
                return ToolOutcome {
                    ok: false,
                    result: json!({"error": format!("the change was refused: {why}"), "denied": reason_str(reason)}),
                    ..Default::default()
                };
            }
            Decision::Allow => (change, DecidedBy::Policy),
            Decision::Ask { reason } => {
                if reason == Reason::OutOfScope {
                    summary.out_of_scope += 1;
                }
                let request = ApprovalRequest {
                    id: id.clone(),
                    tool: name.into(),
                    kind: change.kind().into(),
                    summary: change.summary(),
                    diff: change.diff(),
                    reason: reason_str(reason).into(),
                    change: Some(change.clone()),
                };
                match self.approver.decide(&request).await {
                    ApprovalDecision::Accept => (change, DecidedBy::User),
                    ApprovalDecision::AcceptModified(edited) => {
                        let _ = self.record(Event::other(
                            "user_edit",
                            json!({"proposal_id": id, "summary": edited.summary()}),
                        ));
                        (*edited, DecidedBy::User)
                    }
                    ApprovalDecision::Reject { note } => {
                        let _ = self.record(Event::decision(
                            id,
                            ProposalDecision::Reject,
                            DecidedBy::User,
                        ));
                        let mut result =
                            json!({"error": "the user rejected this proposal", "rejected": true});
                        if let Some(note) = note {
                            result["user_note"] = json!(note);
                        }
                        return ToolOutcome {
                            ok: false,
                            result,
                            ..Default::default()
                        };
                    }
                }
            }
        };
        let modified = by == DecidedBy::User && slide_ids != accepted.slide_ids();
        match dispatch::apply(&ws, &mut self.state, &accepted).await {
            Ok(mut result) => {
                let _ = self.record(Event::decision(id, ProposalDecision::Accept, by));
                if matches!(accepted, Change::SlideCreate { .. }) {
                    self.new_slides_left = self.new_slides_left.saturating_sub(1);
                    for sid in accepted.slide_ids() {
                        self.scope.add(sid);
                    }
                }
                match &accepted {
                    Change::CodeEdit { path, .. } => changed.push(path.clone()),
                    Change::ModeSwitch { .. } => self.state.template = None,
                    _ => changed.extend(accepted.slide_ids()),
                }
                if by == DecidedBy::User {
                    result["decided_by"] = json!("user");
                    if modified || matches!(decision, Decision::Ask { .. }) {
                        if let (
                            Change::SlideUpdate { after, .. }
                            | Change::SlideCreate { slide: after, .. },
                            true,
                        ) = (&accepted, true)
                        {
                            result["slide_as_accepted"] = json!(dispatch::slide_text(after));
                        }
                    }
                }
                ToolOutcome::ok(result)
            }
            Err(message) => {
                let _ = self.record(Event::decision(
                    id,
                    ProposalDecision::Reject,
                    DecidedBy::Policy,
                ));
                let _ = call_id;
                ToolOutcome::err(message)
            }
        }
    }
}

fn compact_args(args: &Value) -> String {
    let text = serde_json::to_string(args).unwrap_or_default();
    if text.chars().count() > 160 {
        format!("{}…", text.chars().take(160).collect::<String>())
    } else {
        text
    }
}
