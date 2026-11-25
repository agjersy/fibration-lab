//! The agent loop (ARCHITECTURE 10): one inspectable loop over a model client and a scene
//! session, with no model-specific branches.
//!
//! The model can read the scene, ask the engine questions, make claims, and propose cells.
//! It cannot change the scene any other way: the tool set is the whole of its authority.
//!
//! * `math_query` runs pure engine operations and records each result as *evidence* with an id.
//! * `claim_make` states a fact and cites evidence ids. The badge is computed here from the
//!   cited results, never taken from the model: **proven** when every cited result was
//!   reached symbolically, **checked** when any was numeric, **unverified** when nothing is
//!   cited or a cited result was undecided.
//! * `scene_propose_cell` goes through [`SceneSession::apply`], which replays the candidate
//!   notebook and keeps it only if every cell runs. A failure goes back to the model with the
//!   classified error; after `max_repairs` failed repairs in a turn no further proposal is
//!   accepted and the model is told to say plainly what could not be done.
//!
//! Model access is `wb-providers`. Review gating of cells under Analyst's strict preset
//! (`wb-policy`) and the shared transcript store (`wb-transcript`) come with the registration
//! of these tools in Analyst's loop (BUILD-SPEC phase 2); standalone, cells apply at once and
//! are undone through the session, and events are written as JSON lines.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wb_providers::model::{ChatRequest, ContentPart, Message, Role, StopReason, ToolDef};
use wb_providers::{collect, ModelClient};

use crate::session::{CellEdit, SceneSession};

pub const SYSTEM_PROMPT: &str = include_str!("prompt.md");

/// Tool results larger than this are cut, with a note saying so.
const MAX_TOOL_RESULT: usize = 12_000;
/// Engine calls per `math_query`.
const MAX_QUERY_CALLS: usize = 16;
/// Handles rendered to text per tool result.
const MAX_RENDERED_HANDLES: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Read-only: discuss and check facts without changing the scene.
    Plan,
    /// Build and change the scene.
    Explore,
}

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub mode: Mode,
    /// Model calls per turn.
    pub max_steps: usize,
    /// Failed cell proposals the model may repair in one turn before proposals stop.
    pub max_repairs: usize,
    pub max_tokens: Option<u32>,
    /// Append every event of every turn here as JSON lines.
    pub transcript: Option<PathBuf>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Explore,
            max_steps: 16,
            max_repairs: 2,
            max_tokens: None,
            transcript: None,
        }
    }
}

/// How a piece of evidence was reached, which decides what it can support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Symbolic,
    Numeric,
    /// The engine answered `unknown`; this supports nothing.
    Undecided,
}

/// One engine result the agent obtained in this conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    #[serde(rename = "fn")]
    pub function: String,
    pub args: Value,
    pub ok: bool,
    pub method: Method,
    /// What the result leaves open, such as a root list the engine could not confirm is
    /// complete. A claim that cites this evidence carries the caveat as its note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caveat: Option<String>,
    /// The engine's value (handles carry their text), or its error.
    pub result: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Proven,
    Checked,
    Unverified,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub text: String,
    pub support: Vec<String>,
    pub status: ClaimStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    User {
        text: String,
    },
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        id: String,
        ok: bool,
        content: String,
    },
    Evidence(Evidence),
    Claim(Claim),
    /// A cell proposal was kept (`ok`) or refused.
    Cell {
        cell: Option<String>,
        caption: String,
        ok: bool,
        error: Option<String>,
    },
    /// The cells run alone in this turn were checked by a replay from the top.
    Verified {
        checked: usize,
        consistent: bool,
    },
    Stop {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    /// The model finished its answer.
    Done,
    /// The step limit was reached with the model still calling tools.
    StepLimit,
    /// The provider reported an error.
    ModelError(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct TurnReport {
    /// The model's prose over the whole turn.
    pub text: String,
    pub claims: Vec<Claim>,
    /// Ids of cells created or changed and kept in this turn.
    pub cells: Vec<String>,
    /// Proposals refused in this turn.
    pub refused: usize,
    pub steps: usize,
    pub end: TurnEnd,
    pub events: Vec<AgentEvent>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The check of the cells run alone in this turn against a replay from the top. `None`
    /// when no cell was run alone. When it is not consistent, some cell does not do the same
    /// thing each time it runs, and the scene is now the replayed one.
    pub verified: Option<crate::Verified>,
}

pub struct Agent {
    client: Arc<dyn ModelClient>,
    pub config: AgentConfig,
    history: Vec<Message>,
    evidence: Vec<Evidence>,
}

/// The method behind an engine value, and what it leaves open.
///
/// Numeric when any part says so; undecided when the answer itself is an unknown truth (or a
/// sign or limit the engine could not determine); symbolic otherwise. An unknown truth under
/// `complete` does not undo the rest of the answer: roots found numerically are still found.
/// It is returned as a caveat instead.
pub fn method_of(function: &str, value: &Value) -> (Method, Option<String>) {
    struct Seen {
        numeric: bool,
        undecided: bool,
        caveat: Option<String>,
    }
    fn walk(key: Option<&str>, v: &Value, seen: &mut Seen) {
        match v {
            Value::Object(map) => {
                if map.get("truth").and_then(Value::as_str) == Some("unknown") {
                    if key == Some("complete") {
                        let reason = map.get("reason").and_then(Value::as_str);
                        seen.caveat = Some(format!(
                            "the engine could not confirm the list is complete{}",
                            reason.map(|r| format!(": {r}")).unwrap_or_default()
                        ));
                    } else {
                        seen.undecided = true;
                    }
                }
                match map.get("method").and_then(Value::as_str) {
                    Some("numeric") => seen.numeric = true,
                    // A sign the engine could not reach by any method.
                    Some("none") if !map.contains_key("truth") => seen.undecided = true,
                    _ => {}
                }
                for (k, child) in map {
                    walk(Some(k), child, seen);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(key, i, seen)),
            _ => {}
        }
    }
    let mut seen = Seen {
        numeric: function == "numeric",
        undecided: false,
        caveat: None,
    };
    walk(None, value, &mut seen);
    let method = if seen.undecided {
        Method::Undecided
    } else if seen.numeric {
        Method::Numeric
    } else {
        Method::Symbolic
    };
    (method, seen.caveat)
}

/// The badge for a claim citing `support`, from the evidence gathered so far. `Err` when an
/// id does not name a successful result: the model must cite real evidence or none.
pub fn judge(
    evidence: &[Evidence],
    support: &[String],
) -> std::result::Result<(ClaimStatus, Option<String>), String> {
    if support.is_empty() {
        return Ok((
            ClaimStatus::Unverified,
            Some("no engine result is cited".into()),
        ));
    }
    let mut numeric = false;
    let mut caveats: Vec<String> = Vec::new();
    for id in support {
        let Some(item) = evidence.iter().find(|e| &e.id == id) else {
            return Err(format!(
                "`{id}` is not a result of a query in this conversation"
            ));
        };
        if !item.ok {
            return Err(format!("`{id}` failed, so it cannot support a claim"));
        }
        match item.method {
            Method::Undecided => {
                return Ok((
                    ClaimStatus::Unverified,
                    Some(format!(
                        "the engine could not decide `{id}` ({})",
                        item.function
                    )),
                ))
            }
            Method::Numeric => numeric = true,
            Method::Symbolic => {}
        }
        if let Some(c) = &item.caveat {
            caveats.push(format!("{id}: {c}"));
        }
    }
    let note = (!caveats.is_empty()).then(|| caveats.join("; "));
    Ok(if numeric {
        (ClaimStatus::Checked, note)
    } else {
        (ClaimStatus::Proven, note)
    })
}

fn tool(name: &str, description: &str, parameters: Value) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
    }
}

/// The tools offered in a mode. Plan mode has no way to change the scene.
pub fn tools(mode: Mode) -> Vec<ToolDef> {
    let mut out = vec![
        tool(
            "scene_read",
            "The notebook's cells and the current scene state (parameters, objects, notes).",
            json!({"type": "object", "properties": {}}),
        ),
        tool(
            "scene_api",
            "Engine operations: without `names`, every operation with its summary and whether \
             it is available in the current scene; with `names`, their full argument schemas.",
            json!({"type": "object", "properties": {
                "names": {"type": "array", "items": {"type": "string"}}
            }}),
        ),
        tool(
            "math_query",
            "Run pure engine operations to learn facts. Each result gets an id (q1, q2, ...) \
             that claims can cite. Cannot change the scene.",
            json!({"type": "object", "required": ["calls"], "properties": {
                "calls": {"type": "array", "minItems": 1, "maxItems": MAX_QUERY_CALLS, "items": {
                    "type": "object", "required": ["fn"], "properties": {
                        "fn": {"type": "string", "description": "Operation name, e.g. limit"},
                        "args": {"type": "object"}
                    }
                }}
            }}),
        ),
        tool(
            "claim_make",
            "State a mathematical fact to the user, citing the query ids that support it. The \
             app computes the badge (proven, checked, unverified) from the cited results.",
            json!({"type": "object", "required": ["text", "support"], "properties": {
                "text": {"type": "string"},
                "support": {"type": "array", "items": {"type": "string"}}
            }}),
        ),
    ];
    if mode == Mode::Explore {
        out.push(tool(
            "scene_propose_cell",
            "Add a cell to the notebook (or replace one). It is replayed from the top and kept \
             only if every cell runs; otherwise nothing changes and the error is returned.",
            json!({"type": "object", "required": ["code", "caption"], "properties": {
                "code": {"type": "string", "description": "Python using the Fibration Math SDK"},
                "caption": {"type": "string", "description": "One line: what the cell does"},
                "after": {"type": "string", "description": "Insert after this cell id (default: at the end)"},
                "replace": {"type": "string", "description": "Replace this cell id instead of adding"}
            }}),
        ));
        out.push(tool(
            "scene_delete_cell",
            "Delete a cell. Kept only if the remaining cells still run.",
            json!({"type": "object", "required": ["id"], "properties": {"id": {"type": "string"}}}),
        ));
    }
    out
}

fn clip(mut text: String) -> String {
    if text.len() > MAX_TOOL_RESULT {
        let mut cut = MAX_TOOL_RESULT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[result truncated]");
    }
    text
}

fn handle_ids(value: &Value, out: &mut Vec<u64>) {
    match value {
        Value::Object(map) => {
            if let Some(h) = map.get("$h").and_then(Value::as_u64) {
                if !out.contains(&h) {
                    out.push(h);
                }
            }
            map.values().for_each(|v| handle_ids(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| handle_ids(v, out)),
        _ => {}
    }
}

fn attach_text(value: &mut Value, texts: &[(u64, String)]) {
    match value {
        Value::Object(map) => {
            if let Some(h) = map.get("$h").and_then(Value::as_u64) {
                if let Some((_, text)) = texts.iter().find(|(id, _)| *id == h) {
                    map.insert("text".into(), Value::String(text.clone()));
                }
            }
            map.values_mut().for_each(|v| attach_text(v, texts));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| attach_text(v, texts)),
        _ => {}
    }
}

/// A compact view of the scene state for the model.
fn state_summary(state: &Value) -> Value {
    let brief = |items: &Value| -> Vec<Value> {
        items
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|o| {
                        let exprs: serde_json::Map<String, Value> = o["exprs"]
                            .as_object()
                            .map(|m| {
                                m.iter()
                                    .map(|(k, v)| (k.clone(), v["text"].clone()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut spec = o["spec"].clone();
                        if o["kind"] == "data" {
                            // Never inline a data series; say how long it is.
                            let n = spec["x"].as_array().map(Vec::len).unwrap_or(0);
                            spec = json!({"points": n, "mode": spec["mode"]});
                        }
                        json!({"id": o["id"], "kind": o["kind"], "exprs": exprs, "spec": spec,
                               "cell": o["origin"]})
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    json!({
        "revision": state["revision"],
        "params": state["params"],
        "objects": brief(&state["objects"]),
        "notes": brief(&state["notes"]),
        "view": state["view"],
    })
}

impl Agent {
    pub fn new(client: Arc<dyn ModelClient>, config: AgentConfig) -> Self {
        Self {
            client,
            config,
            history: Vec::new(),
            evidence: Vec::new(),
        }
    }

    /// Every engine result gathered in this conversation.
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    fn scene_context(session: &SceneSession) -> String {
        let cells: Vec<Value> = session
            .notebook()
            .cells
            .iter()
            .map(|c| json!({"id": c.id, "caption": c.caption, "code": c.code}))
            .collect();
        format!(
            "<scene>\n{}\n</scene>",
            json!({"cells": cells, "state": state_summary(session.state())})
        )
    }

    /// Run one user turn to completion.
    pub async fn turn(&mut self, session: &mut SceneSession, user: &str) -> Result<TurnReport> {
        let mut events = vec![AgentEvent::User {
            text: user.to_string(),
        }];
        // The scene as it is now, so the model reasons over state rather than history.
        self.history.push(Message::user_text(format!(
            "{}\n\n{user}",
            Self::scene_context(session)
        )));

        let tools = tools(self.config.mode);
        let mut report = TurnReport {
            text: String::new(),
            claims: Vec::new(),
            cells: Vec::new(),
            refused: 0,
            steps: 0,
            end: TurnEnd::StepLimit,
            events: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            verified: None,
        };
        let mut failures_in_a_row = 0usize;

        while report.steps < self.config.max_steps {
            report.steps += 1;
            let request = ChatRequest {
                model: self.client.model_id().to_string(),
                system: Some(SYSTEM_PROMPT.to_string()),
                messages: self.history.clone(),
                tools: tools.clone(),
                max_tokens: self.config.max_tokens,
                audit_tag: Some("fibration_math.turn".to_string()),
            };
            let reply = collect(self.client.stream(request)).await;
            report.input_tokens += reply.input_tokens;
            report.output_tokens += reply.output_tokens;

            let mut content: Vec<ContentPart> = Vec::new();
            if !reply.text.trim().is_empty() {
                content.push(ContentPart::Text {
                    text: reply.text.clone(),
                });
                events.push(AgentEvent::Text {
                    text: reply.text.clone(),
                });
                if !report.text.is_empty() {
                    report.text.push_str("\n\n");
                }
                report.text.push_str(reply.text.trim());
            }
            for (id, name, arguments) in &reply.tool_calls {
                content.push(ContentPart::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
            }
            if !content.is_empty() {
                self.history.push(Message {
                    role: Role::Assistant,
                    content,
                });
            }

            if reply.stop == Some(StopReason::Error) {
                let message = reply
                    .stop_message
                    .unwrap_or_else(|| "the model call failed".into());
                report.end = TurnEnd::ModelError(message);
                break;
            }
            if reply.tool_calls.is_empty() {
                report.end = TurnEnd::Done;
                break;
            }

            let mut results: Vec<ContentPart> = Vec::new();
            for (id, name, arguments) in reply.tool_calls {
                events.push(AgentEvent::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
                let outcome = self
                    .run_tool(
                        session,
                        &name,
                        &arguments,
                        &mut report,
                        &mut events,
                        &mut failures_in_a_row,
                    )
                    .await;
                let (ok, content) = match outcome {
                    Ok(value) => (true, clip(value.to_string())),
                    Err(message) => (false, clip(message)),
                };
                events.push(AgentEvent::ToolResult {
                    id: id.clone(),
                    ok,
                    content: content.clone(),
                });
                results.push(ContentPart::ToolResult {
                    tool_call_id: id,
                    content,
                    is_error: !ok,
                });
            }
            self.history.push(Message {
                role: Role::User,
                content: results,
            });
        }

        // Cells appended in this turn ran alone; the notebook replayed from the top is the
        // definition, so check them against it before the turn is reported.
        let verified = session.verify().await?;
        if verified.checked > 0 {
            events.push(AgentEvent::Verified {
                checked: verified.checked,
                consistent: verified.consistent,
            });
            report.verified = Some(verified);
        }

        events.push(AgentEvent::Stop {
            reason: match &report.end {
                TurnEnd::Done => "done".into(),
                TurnEnd::StepLimit => "step_limit".into(),
                TurnEnd::ModelError(m) => format!("model_error: {m}"),
            },
        });
        self.write_transcript(&events);
        report.events = events;
        Ok(report)
    }

    fn write_transcript(&self, events: &[AgentEvent]) {
        let Some(path) = &self.config.transcript else {
            return;
        };
        let written = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            for event in events {
                writeln!(file, "{}", serde_json::to_string(event)?)?;
            }
            Ok(())
        })();
        if let Err(e) = written {
            tracing::warn!("transcript write failed: {e}");
        }
    }

    /// Execute one tool call. `Err` is the message returned to the model as a tool error.
    async fn run_tool(
        &mut self,
        session: &mut SceneSession,
        name: &str,
        args: &Value,
        report: &mut TurnReport,
        events: &mut Vec<AgentEvent>,
        failures_in_a_row: &mut usize,
    ) -> std::result::Result<Value, String> {
        let explore = self.config.mode == Mode::Explore;
        match name {
            "scene_read" => {
                let cells: Vec<Value> = session
                    .notebook()
                    .cells
                    .iter()
                    .map(|c| json!({"id": c.id, "caption": c.caption, "code": c.code}))
                    .collect();
                Ok(json!({"cells": cells, "state": state_summary(session.state())}))
            }
            "scene_api" => {
                let api = session.api().await.map_err(|e| e.to_string())?;
                let ops = api["operations"].as_array().cloned().unwrap_or_default();
                match args.get("names").and_then(Value::as_array) {
                    Some(names) if !names.is_empty() => {
                        let wanted: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
                        let found: Vec<Value> = ops
                            .into_iter()
                            .filter(|o| o["name"].as_str().is_some_and(|n| wanted.contains(&n)))
                            .collect();
                        if found.is_empty() {
                            return Err("none of those names is an operation; call scene_api without names to list them".into());
                        }
                        Ok(json!({"operations": found}))
                    }
                    _ => Ok(json!({"operations": ops.iter().map(|o| json!({
                        "name": o["name"], "summary": o["summary"], "pure": o["pure"],
                        "available": o["available"]
                    })).collect::<Vec<_>>()})),
                }
            }
            "math_query" => self.math_query(session, args, events).await,
            "claim_make" => {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return Err("a claim needs `text`".into());
                }
                let support: Vec<String> = args
                    .get("support")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let (status, note) = judge(&self.evidence, &support)?;
                let claim = Claim {
                    text,
                    support,
                    status,
                    note,
                };
                events.push(AgentEvent::Claim(claim.clone()));
                report.claims.push(claim.clone());
                Ok(json!({"status": claim.status, "note": claim.note}))
            }
            "scene_propose_cell" | "scene_delete_cell" if !explore => {
                Err("the conversation is in plan mode, which cannot change the scene".into())
            }
            "scene_propose_cell" | "scene_delete_cell" => {
                if *failures_in_a_row > self.config.max_repairs {
                    return Err(
                        "no further changes are accepted in this turn. Tell the user plainly \
                         what could not be done and what is possible instead."
                            .into(),
                    );
                }
                let (edit, caption) = if name == "scene_delete_cell" {
                    let id = args
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or("`id` is required")?;
                    (
                        CellEdit::Delete { id: id.to_string() },
                        format!("delete {id}"),
                    )
                } else {
                    let code = args
                        .get("code")
                        .and_then(Value::as_str)
                        .ok_or("`code` is required")?;
                    let caption = args
                        .get("caption")
                        .and_then(Value::as_str)
                        .ok_or("`caption` is required")?
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let edit = match (
                        args.get("replace").and_then(Value::as_str),
                        args.get("after").and_then(Value::as_str),
                    ) {
                        (Some(id), _) => CellEdit::Replace {
                            id: id.to_string(),
                            code: code.to_string(),
                            caption: Some(caption.clone()),
                        },
                        (None, Some(after)) => CellEdit::InsertAfter {
                            after: Some(after.to_string()),
                            code: code.to_string(),
                            caption: caption.clone(),
                        },
                        (None, None) => CellEdit::Append {
                            code: code.to_string(),
                            caption: caption.clone(),
                        },
                    };
                    (edit, caption)
                };
                // A malformed edit (unknown cell id, a marker line inside the code) is the
                // model's to fix, like a cell that does not run.
                let applied = match session.apply(edit).await {
                    Ok(applied) => applied,
                    Err(e) => {
                        *failures_in_a_row += 1;
                        report.refused += 1;
                        let message = format!("{e:#}");
                        events.push(AgentEvent::Cell {
                            cell: None,
                            caption,
                            ok: false,
                            error: Some(message.clone()),
                        });
                        return Err(message);
                    }
                };
                if applied.ok {
                    *failures_in_a_row = 0;
                    if let Some(cell) = &applied.cell {
                        report.cells.push(cell.clone());
                    }
                    events.push(AgentEvent::Cell {
                        cell: applied.cell.clone(),
                        caption,
                        ok: true,
                        error: None,
                    });
                    let printed = applied
                        .cell
                        .as_ref()
                        .and_then(|id| applied.replay.results.iter().find(|r| &r.id == id))
                        .map(|r| r.stdout.clone())
                        .unwrap_or_default();
                    Ok(json!({
                        "kept": true, "cell": applied.cell, "printed": printed,
                        "state": state_summary(session.state()),
                    }))
                } else {
                    *failures_in_a_row += 1;
                    report.refused += 1;
                    let failure = applied.failure();
                    let detail = failure
                        .and_then(|f| f.error.as_ref())
                        .map(|e| {
                            json!({
                                "cell": failure.map(|f| f.id.clone()),
                                "type": e.kind, "message": e.message, "line": e.line,
                                "engine_error": e.call_error,
                            })
                        })
                        .unwrap_or_else(|| json!({"message": "the cell did not run"}));
                    let left = (self.config.max_repairs + 1).saturating_sub(*failures_in_a_row);
                    events.push(AgentEvent::Cell {
                        cell: applied.cell.clone(),
                        caption,
                        ok: false,
                        error: Some(detail["message"].as_str().unwrap_or("").to_string()),
                    });
                    Err(json!({
                        "kept": false,
                        "error": detail,
                        "note": "nothing changed; the scene is as it was",
                        "attempts_left": left,
                    })
                    .to_string())
                }
            }
            other => Err(format!("there is no tool `{other}`")),
        }
    }

    async fn math_query(
        &mut self,
        session: &SceneSession,
        args: &Value,
        events: &mut Vec<AgentEvent>,
    ) -> std::result::Result<Value, String> {
        let calls = args
            .get("calls")
            .and_then(Value::as_array)
            .filter(|c| !c.is_empty())
            .ok_or("`calls` must be a non-empty list of {fn, args}")?;
        if calls.len() > MAX_QUERY_CALLS {
            return Err(format!("at most {MAX_QUERY_CALLS} calls per query"));
        }
        let mut out: Vec<Value> = Vec::new();
        for call in calls {
            let function = call
                .get("fn")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let call_args = call.get("args").cloned().unwrap_or_else(|| json!({}));
            let id = format!("q{}", self.evidence.len() + 1);
            let (ok, (method, caveat), result) =
                match session.query(&function, call_args.clone(), "agent").await {
                    Err(e) => (
                        false,
                        (Method::Undecided, None),
                        json!({"error": e.to_string()}),
                    ),
                    Ok(reply) if reply["status"] == "ok" => {
                        let mut value = reply["value"].clone();
                        let mut ids = Vec::new();
                        handle_ids(&value, &mut ids);
                        let mut texts = Vec::new();
                        for h in ids.into_iter().take(MAX_RENDERED_HANDLES) {
                            if let Ok(t) = session
                                .query("to_text", json!({"of": {"$h": h}}), "agent")
                                .await
                            {
                                if let Some(s) = t["value"].as_str() {
                                    texts.push((h, s.to_string()));
                                }
                            }
                        }
                        attach_text(&mut value, &texts);
                        (true, method_of(&function, &value), value)
                    }
                    Ok(reply) => (
                        false,
                        (Method::Undecided, None),
                        json!({"error": reply["error"]}),
                    ),
                };
            let item = Evidence {
                id: id.clone(),
                function,
                args: call_args,
                ok,
                method,
                caveat: caveat.clone(),
                result: result.clone(),
            };
            events.push(AgentEvent::Evidence(item.clone()));
            self.evidence.push(item);
            out.push(
                json!({"id": id, "ok": ok, "method": method, "caveat": caveat, "result": result}),
            );
        }
        Ok(json!({"results": out}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, ok: bool, method: Method) -> Evidence {
        Evidence {
            id: id.into(),
            function: "limit".into(),
            args: json!({}),
            ok,
            method,
            caveat: None,
            result: json!({}),
        }
    }

    #[test]
    fn method_follows_the_engine_result() {
        let m = |f, v: Value| method_of(f, &v).0;
        assert_eq!(
            m(
                "limit",
                json!({"exists": {"truth": "true", "method": "symbolic"}})
            ),
            Method::Symbolic
        );
        assert_eq!(
            m("diff", json!({"$h": 3, "kind": "expr"})),
            Method::Symbolic
        );
        // A limit the engine could not evaluate decides nothing.
        assert_eq!(
            m(
                "limit",
                json!({"value": null, "exists": {"truth": "unknown", "method": "none"}})
            ),
            Method::Undecided
        );
        assert_eq!(
            m("sign_on", json!({"sign": "mixed", "method": "numeric"})),
            Method::Numeric
        );
        assert_eq!(
            m("sign_on", json!({"sign": "unknown", "method": "none"})),
            Method::Undecided
        );
        assert_eq!(m("numeric", json!({"value": 2.5})), Method::Numeric);
        assert_eq!(
            m("equals", json!({"truth": "unknown", "method": "none"})),
            Method::Undecided
        );
        // Roots found numerically are found; only their completeness is open.
        let (method, caveat) = method_of(
            "roots_in",
            &json!({"roots": [{"approx": 0.739}], "method": "numeric",
                    "complete": {"truth": "unknown", "method": "none", "reason": "sign changes only"}}),
        );
        assert_eq!(method, Method::Numeric);
        assert!(caveat.unwrap().contains("sign changes only"));
    }

    #[test]
    fn the_badge_is_computed_from_cited_evidence_only() {
        let evidence = vec![
            ev("q1", true, Method::Symbolic),
            ev("q2", true, Method::Numeric),
            ev("q3", true, Method::Undecided),
            ev("q4", false, Method::Undecided),
        ];
        let s = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            judge(&evidence, &s(&["q1"])).unwrap().0,
            ClaimStatus::Proven
        );
        // One numeric result is enough to lose "proven".
        assert_eq!(
            judge(&evidence, &s(&["q1", "q2"])).unwrap().0,
            ClaimStatus::Checked
        );
        assert_eq!(
            judge(&evidence, &s(&["q1", "q3"])).unwrap().0,
            ClaimStatus::Unverified
        );
        assert_eq!(judge(&evidence, &[]).unwrap().0, ClaimStatus::Unverified);
        // A caveat on cited evidence travels with the claim.
        let mut open = ev("q5", true, Method::Numeric);
        open.caveat = Some("the list may be incomplete".into());
        let (status, note) = judge(&[open], &s(&["q5"])).unwrap();
        assert_eq!(status, ClaimStatus::Checked);
        assert!(note.unwrap().contains("q5: the list may be incomplete"));
        assert!(judge(&evidence, &s(&["q9"])).is_err());
        assert!(judge(&evidence, &s(&["q4"])).is_err());
    }

    #[test]
    fn plan_mode_cannot_change_the_scene() {
        let names = |m| tools(m).into_iter().map(|t| t.name).collect::<Vec<_>>();
        assert!(!names(Mode::Plan)
            .iter()
            .any(|n| n.starts_with("scene_propose") || n.starts_with("scene_delete")));
        assert!(names(Mode::Explore).contains(&"scene_propose_cell".to_string()));
        assert!(tools(Mode::Explore)
            .iter()
            .all(|t| wb_providers::names::is_valid_model_name(&t.name)));
    }
}
