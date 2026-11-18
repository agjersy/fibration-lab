//! W12 acceptance: the agent loop driven by a scripted model against the real kernel.

use analyst_deck::agent::dispatch::Change;
use analyst_deck::agent::tools::Mode;
use analyst_deck::agent::{
    Agent, AgentConfig, ApprovalDecision, ApprovalRequest, Approver, AutoApprover,
};
use analyst_deck::deck_store::Position;
use analyst_deck::project::Project;
use analyst_deck::workspace::{OpenOptions, Workspace};
use analyst_deck_protocol::deck::{DeckMode, Slide};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use wb_policy::{EditGate, Level, Policy};
use wb_providers::model::{ContentPart, ModelCapabilities, ModelEvent, StopReason};
use wb_providers::ScriptedModelClient;
use wb_transcript::TranscriptStore;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn caps(vision: bool) -> ModelCapabilities {
    serde_json::from_value(json!({"context_window": 200000, "tools": "native", "vision": vision, "probed_at": "2026-11-15T00:00:00Z"})).unwrap()
}

fn call(id: &str, name: &str, args: Value) -> ModelEvent {
    ModelEvent::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

fn text(t: &str) -> ModelEvent {
    ModelEvent::Text { delta: t.into() }
}

fn stop_tools() -> ModelEvent {
    ModelEvent::Stop {
        reason: StopReason::ToolCalls,
        message: None,
    }
}

fn slide_doc(id: Option<&str>, title: &str, body: &[&str]) -> Value {
    let mut doc = json!({
        "layout": "Title and Content",
        "placeholders": {
            "title": {"type": "text", "text": {"paragraphs": [{"runs": [{"t": title}]}]}},
            "body": {"type": "text", "text": {"paragraphs": body.iter().map(|b| json!({"runs": [{"t": b}]})).collect::<Vec<_>>()}}
        }
    });
    if let Some(id) = id {
        doc["id"] = json!(id);
    }
    doc
}

struct Recording {
    decisions: Mutex<Vec<ApprovalDecision>>,
    seen: Mutex<Vec<ApprovalRequest>>,
}

impl Recording {
    fn new(decisions: Vec<ApprovalDecision>) -> Arc<Recording> {
        Arc::new(Recording {
            decisions: Mutex::new(decisions),
            seen: Mutex::new(Vec::new()),
        })
    }
    fn requests(&self) -> Vec<ApprovalRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Approver for Recording {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.seen.lock().unwrap().push(request.clone());
        let mut d = self.decisions.lock().unwrap();
        if d.is_empty() {
            ApprovalDecision::Accept
        } else {
            d.remove(0)
        }
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    ws: Arc<Workspace>,
    store: TranscriptStore,
}

async fn rig(template: &str) -> Option<Rig> {
    let tmp = tempfile::tempdir().unwrap();
    let file = match template {
        "corp" => "evals/templates/corp/corp.potx",
        "wide-dense" => "evals/templates/wide-dense/wide-dense.pptx",
        _ => "evals/templates/plain/plain.pptx",
    };
    let project = Project::create(
        &tmp.path().join("p"),
        "Agent test",
        &repo().join(file),
        DeckMode::Structured,
    )
    .unwrap();
    let mut options = OpenOptions::new(&tmp.path().join("home"));
    options.engine = "none".into();
    options.kernel_env = vec![(
        "PRESENT_FONT_DIRS".into(),
        repo().join("vendor/fonts").to_string_lossy().into_owned(),
    )];
    let ws = match Workspace::open(&project.root, &options).await {
        Ok(ws) => Arc::new(ws),
        Err(e) if e.to_string().contains("no kernel interpreter") => {
            eprintln!("skipped: {e}");
            return None;
        }
        Err(e) => panic!("{e:?}"),
    };
    let store = TranscriptStore::new(tmp.path().join("home").join("projects").join("p1"));
    Some(Rig {
        _tmp: tmp,
        ws,
        store,
    })
}

fn agent(
    rig: &Rig,
    scripts: Vec<Vec<ModelEvent>>,
    policy: Policy,
    approver: Arc<dyn Approver>,
    mode: Mode,
) -> (Agent, Arc<ScriptedModelClient>) {
    let model = Arc::new(ScriptedModelClient::new(scripts));
    let config = AgentConfig::new(mode, "scripted/model", caps(false), 20);
    (
        Agent::new(
            model.clone(),
            rig.ws.clone(),
            policy,
            approver,
            config,
            rig.store.clone(),
            "c-1",
        ),
        model,
    )
}

fn seed(rig: &Rig, id: &str, title: &str, locked: bool) -> Slide {
    let mut doc = slide_doc(Some(id), title, &["An existing point"]);
    doc["kind"] = json!("layout");
    doc["elements"] = json!([]);
    doc["placeholders"]["title"]["id"] = json!(format!("e-t{}", &id[2..]));
    doc["placeholders"]["body"]["id"] = json!(format!("e-b{}", &id[2..]));
    if locked {
        doc["locked"] = json!(true);
    }
    let slide: Slide = serde_json::from_value(doc).unwrap();
    rig.ws
        .deck()
        .create_slide(&slide, &Position::default())
        .unwrap();
    slide
}

fn tool_results(model: &ScriptedModelClient, request: usize) -> Vec<(bool, Value)> {
    let recorded = model.recorded();
    recorded[request]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::ToolResult {
                content, is_error, ..
            } => Some((
                !is_error,
                serde_json::from_str(content).unwrap_or(Value::Null),
            )),
            _ => None,
        })
        .collect()
}

fn review() -> Policy {
    Policy::new().with_edits(EditGate::Review)
}

fn auto() -> Policy {
    Policy::new()
        .with_edits(EditGate::Auto)
        .with_class("export", Level::Auto)
}

#[tokio::test]
async fn review_mode_pauses_then_accept_reject_and_accept_modified() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    let scripts = vec![
        vec![
            call(
                "1",
                "scope_declare",
                json!({"slide_ids": [], "new_slides": 3, "plan": "Add three slides"}),
            ),
            stop_tools(),
        ],
        vec![
            call(
                "2",
                "slide_propose_create",
                json!({"slide": slide_doc(Some("s-new1"), "First slide is accepted", &["A point"])}),
            ),
            stop_tools(),
        ],
        vec![
            call(
                "3",
                "slide_propose_create",
                json!({"slide": slide_doc(Some("s-new2"), "Second slide is rejected", &["A point"])}),
            ),
            stop_tools(),
        ],
        vec![
            call(
                "4",
                "slide_propose_create",
                json!({"slide": slide_doc(Some("s-new3"), "Third slide is edited", &["A point"])}),
            ),
            stop_tools(),
        ],
        vec![text("Done: two slides added, one rejected.")],
    ];
    let edited: Slide = serde_json::from_value({
        let mut d = slide_doc(
            Some("s-new3"),
            "Third slide, as the user rewrote it",
            &["The user's point"],
        );
        d["kind"] = json!("layout");
        d["elements"] = json!([]);
        d["placeholders"]["title"]["id"] = json!("e-ut");
        d["placeholders"]["body"]["id"] = json!("e-ub");
        d
    })
    .unwrap();
    let approver = Recording::new(vec![
        ApprovalDecision::Accept,
        ApprovalDecision::Reject {
            note: Some("too vague".into()),
        },
        ApprovalDecision::AcceptModified(Box::new(Change::SlideCreate {
            slide: Box::new(edited),
            after: None,
            section: None,
        })),
    ]);
    let (mut agent, model) = agent(&rig, scripts, review(), approver.clone(), Mode::Build);
    let summary = agent.run_turn("Add three slides").await.unwrap();
    let store = rig.ws.deck();
    let ids: Vec<String> = store
        .read_deck()
        .unwrap()
        .slide_ids()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(ids, ["s-new1", "s-new3"]);
    assert!(!store.slide_path_exists("s-new2"));
    let (third, _) = store.read_slide(&"s-new3".parse().unwrap()).unwrap();
    assert_eq!(
        analyst_deck::agent::dispatch::title_of(&third),
        "Third slide, as the user rewrote it"
    );
    let requests = approver.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|r| r.kind == "slide-create" && r.reason == "policy"));
    assert!(requests[0].diff.contains("+ First slide is accepted"));
    // The model was told about the rejection, with the user's note, and about the user's edit.
    let rejected = &tool_results(&model, 3)[0];
    assert!(
        !rejected.0 && rejected.1["rejected"] == true && rejected.1["user_note"] == "too vague"
    );
    let modified = &tool_results(&model, 4)[0];
    assert!(
        modified.0
            && modified.1["slide_as_accepted"]
                .as_str()
                .unwrap()
                .contains("as the user rewrote it")
    );
    assert_eq!(summary.slides_changed, ["s-new1", "s-new3"]);
    assert_eq!(summary.text, "Done: two slides added, one rejected.");
    // One-click revert of the turn restores the empty deck.
    rig.ws.snapshots().revert_turn(&summary.turn_id).unwrap();
    assert_eq!(store.read_deck().unwrap().slide_ids().count(), 0);
    // The transcript recorded proposals and decisions.
    let events = rig.store.read("c-1").unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.type_name()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "proposal").count(), 3);
    assert_eq!(kinds.iter().filter(|k| **k == "decision").count(), 3);
    assert_eq!(kinds.last(), Some(&"summary"));
}

#[tokio::test]
async fn auto_mode_applies_in_scope_and_asks_out_of_scope() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    seed(&rig, "s-aaaa", "Slide A", false);
    seed(&rig, "s-bbbb", "Slide B", false);
    let scripts = vec![
        vec![
            call(
                "1",
                "scope_declare",
                json!({"slide_ids": ["s-aaaa"], "new_slides": 0, "plan": "Reword slide A"}),
            ),
            stop_tools(),
        ],
        vec![
            call(
                "2",
                "slide_propose_update",
                json!({"slide_id": "s-aaaa", "patch": [{"op": "replace", "path": "/placeholders/title/text/paragraphs/0/runs/0/t", "value": "Slide A, reworded"}]}),
            ),
            call(
                "3",
                "slide_propose_update",
                json!({"slide_id": "s-bbbb", "patch": [{"op": "replace", "path": "/placeholders/title/text/paragraphs/0/runs/0/t", "value": "Slide B, changed without being asked"}]}),
            ),
            call(
                "4",
                "slide_propose_create",
                json!({"slide": slide_doc(Some("s-cccc"), "An undeclared new slide", &["x"])}),
            ),
            stop_tools(),
        ],
        vec![text("Done.")],
    ];
    let approver = Recording::new(vec![
        ApprovalDecision::Reject { note: None },
        ApprovalDecision::Reject { note: None },
    ]);
    let (mut agent, _model) = agent(&rig, scripts, auto(), approver.clone(), Mode::Build);
    let summary = agent.run_turn("Reword slide A").await.unwrap();
    let store = rig.ws.deck();
    let title = |id: &str| {
        analyst_deck::agent::dispatch::title_of(&store.read_slide(&id.parse().unwrap()).unwrap().0)
    };
    assert_eq!(title("s-aaaa"), "Slide A, reworded");
    assert_eq!(title("s-bbbb"), "Slide B");
    assert!(!store.slide_path_exists("s-cccc"));
    let requests = approver.requests();
    assert_eq!(
        requests.len(),
        2,
        "only the out-of-scope changes reached the user"
    );
    assert!(requests.iter().all(|r| r.reason == "out_of_scope"));
    assert_eq!(summary.out_of_scope, 2);
    assert_eq!(summary.slides_changed, ["s-aaaa"]);
}

#[tokio::test]
async fn locked_slides_and_elements_are_never_changed() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    seed(&rig, "s-lock", "Locked slide", true);
    let mut partly = serde_json::to_value(seed(&rig, "s-part", "Partly locked", false)).unwrap();
    partly["locked_elements"] = json!(["e-tpart"]);
    rig.ws
        .deck()
        .update_slide(&serde_json::from_value(partly).unwrap(), None)
        .unwrap();
    let patch = |id: &str, path: &str| json!({"slide_id": id, "patch": [{"op": "replace", "path": path, "value": "changed"}]});
    let scripts = vec![
        vec![
            call(
                "1",
                "scope_declare",
                json!({"slide_ids": ["s-lock", "s-part"], "plan": "try"}),
            ),
            call(
                "2",
                "slide_propose_update",
                patch("s-lock", "/placeholders/title/text/paragraphs/0/runs/0/t"),
            ),
            call("3", "slide_propose_delete", json!({"slide_id": "s-lock"})),
            call(
                "4",
                "slide_propose_update",
                patch("s-part", "/placeholders/title/text/paragraphs/0/runs/0/t"),
            ),
            call(
                "5",
                "slide_propose_update",
                patch("s-part", "/placeholders/body/text/paragraphs/0/runs/0/t"),
            ),
            stop_tools(),
        ],
        vec![text("Slide s-lock is locked.")],
    ];
    let approver = Recording::new(vec![]);
    let (mut agent, model) = agent(&rig, scripts, auto(), approver.clone(), Mode::Build);
    let summary = agent.run_turn("Change everything").await.unwrap();
    let results = tool_results(&model, 1);
    assert_eq!(
        results.iter().map(|r| r.0).collect::<Vec<_>>(),
        [true, false, false, false, true]
    );
    assert_eq!(results[1].1["denied"], "locked");
    assert!(
        approver.requests().is_empty(),
        "a locked target is refused, not sent for approval"
    );
    assert_eq!(summary.slides_changed, ["s-part"]);
    let store = rig.ws.deck();
    assert_eq!(
        analyst_deck::agent::dispatch::title_of(
            &store.read_slide(&"s-lock".parse().unwrap()).unwrap().0
        ),
        "Locked slide"
    );
    assert_eq!(
        analyst_deck::agent::dispatch::title_of(
            &store.read_slide(&"s-part".parse().unwrap()).unwrap().0
        ),
        "Partly locked"
    );
}

#[tokio::test]
async fn polish_mode_rejects_new_slides_and_new_claims() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    seed(&rig, "s-aaaa", "Slide A", false);
    let scripts = vec![
        vec![
            call(
                "1",
                "scope_declare",
                json!({"slide_ids": ["s-aaaa"], "plan": "polish"}),
            ),
            call(
                "2",
                "slide_propose_create",
                json!({"slide": slide_doc(None, "A new slide", &["x"])}),
            ),
            call(
                "3",
                "slide_propose_update",
                json!({"slide_id": "s-aaaa", "patch": [{"op": "add", "path": "/placeholders/body/text/paragraphs/0/runs/0/cite", "value": ["ref:r-new@000000000000#p1"]}]}),
            ),
            call(
                "4",
                "slide_propose_update",
                json!({"slide_id": "s-aaaa", "patch": [{"op": "replace", "path": "/placeholders/body/text/paragraphs/0/runs/0/t", "value": "A crisper point"}]}),
            ),
            stop_tools(),
        ],
        vec![text("Polished.")],
    ];
    let (mut agent, model) = agent(&rig, scripts, auto(), Arc::new(AutoApprover), Mode::Polish);
    agent.run_turn("Tighten the wording").await.unwrap();
    let offered: Vec<String> = model.recorded()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(
        !offered.contains(&"slide_propose_create".to_string())
            && !offered.contains(&"slide_propose_delete".to_string())
    );
    let results = tool_results(&model, 1);
    assert_eq!(
        results.iter().map(|r| r.0).collect::<Vec<_>>(),
        [true, false, false, true]
    );
    assert!(results[1].1["error"]
        .as_str()
        .unwrap()
        .contains("not available in polish mode"));
    assert_eq!(results[2].1["denied"], "mode");
    let (slide, _) = rig
        .ws
        .deck()
        .read_slide(&"s-aaaa".parse().unwrap())
        .unwrap();
    assert!(analyst_deck::agent::dispatch::slide_text(&slide).contains("A crisper point"));
    assert_eq!(rig.ws.deck().read_deck().unwrap().slide_ids().count(), 1);
}

#[tokio::test]
async fn checker_findings_come_back_and_the_model_fixes_an_overflow() {
    let Some(rig) = rig("wide-dense").await else {
        return;
    };
    let long: Vec<String> = (0..40)
        .map(|i| format!("Supporting point {i} that goes on for a while to fill the line and wrap"))
        .collect();
    let long_refs: Vec<&str> = long.iter().map(String::as_str).collect();
    let scripts = vec![
        vec![
            call(
                "1",
                "scope_declare",
                json!({"slide_ids": [], "new_slides": 1, "plan": "One slide"}),
            ),
            stop_tools(),
        ],
        vec![
            call(
                "2",
                "slide_propose_create",
                json!({"slide": slide_doc(Some("s-over"), "A slide with far too much text", &long_refs)}),
            ),
            stop_tools(),
        ],
        vec![
            call(
                "3",
                "slide_propose_update",
                json!({"slide_id": "s-over", "slide": slide_doc(None, "A slide with the right amount of text", &["Three points", "that fit", "the box"])}),
            ),
            stop_tools(),
        ],
        vec![text("Fixed the overflow.")],
    ];
    let (mut agent, model) = agent(&rig, scripts, auto(), Arc::new(AutoApprover), Mode::Build);
    let summary = agent.run_turn("Make a slide").await.unwrap();
    let feedback = model.recorded()[2]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .find_map(|p| match p {
            ContentPart::Text { text } if text.contains("[auto-check round 1/3]") => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("the checker's findings were fed back after the first change");
    assert!(feedback.contains("fit.overflow") && feedback.contains("s-over"));
    let after_fix = model.recorded()[3]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Text { text } if text.contains("[auto-check")));
    assert!(!after_fix, "no findings after the fix");
    assert!(summary.open_issues.is_empty());
    let diags = rig
        .ws
        .check(analyst_deck::agent::dispatch::AGENT_PPTX, None, None, false)
        .await
        .unwrap();
    assert!(!diags.iter().any(|d| d["rule"] == "fit.overflow"));
}

#[tokio::test]
async fn invalid_slides_are_explained_to_the_model_not_written() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    let mut bad_layout = slide_doc(None, "x", &["y"]);
    bad_layout["layout"] = json!("No Such Layout");
    let mut bad_key = slide_doc(None, "x", &["y"]);
    bad_key["placeholders"]["sidebar"] = json!({"type": "text", "text": {"paragraphs": []}});
    let mut bad_shape = slide_doc(None, "x", &["y"]);
    bad_shape["placeholders"]["body"] = json!({"type": "text", "text": "just a string"});
    let scripts = vec![
        vec![
            call(
                "1",
                "scope_declare",
                json!({"slide_ids": [], "new_slides": 3, "plan": "p"}),
            ),
            call("2", "slide_propose_create", json!({"slide": bad_layout})),
            call("3", "slide_propose_create", json!({"slide": bad_key})),
            call("4", "slide_propose_create", json!({"slide": bad_shape})),
            call("5", "no_such_tool", json!({})),
            stop_tools(),
        ],
        vec![text("ok")],
    ];
    let (mut agent, model) = agent(&rig, scripts, auto(), Arc::new(AutoApprover), Mode::Build);
    let summary = agent.run_turn("go").await.unwrap();
    let results = tool_results(&model, 1);
    let errors: Vec<&str> = results[1..]
        .iter()
        .map(|r| r.1["error"].as_str().unwrap())
        .collect();
    assert!(
        errors[0].contains("no layout \"No Such Layout\"")
            && errors[0].contains("Title and Content")
    );
    assert!(errors[1].contains("no placeholder \"sidebar\"") && errors[1].contains("title, body"));
    assert!(errors[2].contains("not valid"));
    assert!(errors[3].contains("unknown tool"));
    assert_eq!(summary.tool_errors, 4);
    assert_eq!(rig.ws.deck().read_deck().unwrap().slide_ids().count(), 0);
}

#[tokio::test]
async fn outline_mode_proposes_a_storyline_and_stores_the_accepted_one() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    let rows = json!([
        {"title": "Margin recovered to 18% on pricing actions", "message": "Pricing drove the recovery", "layout": "Title and Content"},
        {"title": "EMEA delivered two thirds of the gain", "message": "Regional split", "layout": "Two Content"}
    ]);
    let scripts = vec![
        vec![
            call("1", "outline_propose", json!({"rows": rows})),
            stop_tools(),
        ],
        vec![text("Outline proposed.")],
    ];
    let approver = Recording::new(vec![]);
    let (mut agent, model) = agent(&rig, scripts, review(), approver.clone(), Mode::Outline);
    let summary = agent.run_turn("Storyline for the Q3 review").await.unwrap();
    assert_eq!(approver.requests()[0].kind, "outline");
    assert!(
        summary.snapshot_id.is_none(),
        "outline turns change no slides and take no snapshot"
    );
    let stored: Value = serde_json::from_str(
        &std::fs::read_to_string(rig.ws.project.root.join(".present/outline.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored.as_array().unwrap().len(), 2);
    let system = model.recorded()[0].system.clone().unwrap();
    assert!(
        system.contains("MODE outline")
            && system.contains("PROJECT MAP")
            && system.contains("Title and Content")
    );
    assert!(!model.recorded()[0]
        .tools
        .iter()
        .any(|t| t.name == "slide_propose_create"));
    // The next build turn sees the approved outline in its project map.
    let (mut build, model2) = agent_with_conv(&rig, vec![vec![text("ok")]], "c-2");
    build.run_turn("Build it").await.unwrap();
    assert!(model2.recorded()[0]
        .system
        .clone()
        .unwrap()
        .contains("approved_outline"));
}

fn agent_with_conv(
    rig: &Rig,
    scripts: Vec<Vec<ModelEvent>>,
    conv: &str,
) -> (Agent, Arc<ScriptedModelClient>) {
    let model = Arc::new(ScriptedModelClient::new(scripts));
    let config = AgentConfig::new(Mode::Build, "scripted/model", caps(false), 20);
    (
        Agent::new(
            model.clone(),
            rig.ws.clone(),
            auto(),
            Arc::new(AutoApprover),
            config,
            rig.store.clone(),
            conv,
        ),
        model,
    )
}

#[tokio::test]
async fn skills_are_listed_by_description_loaded_on_demand_and_scripts_ask() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    let dir = rig.ws.project.root.join(".present/skills/board-deck");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: board-deck\ndescription: House rules for board decks. Use when the audience is the board.\n---\n# Board decks\n\nAlways open with the decision being asked for. SECRET-BODY-MARKER\n").unwrap();
    std::fs::write(dir.join("scripts/hello.py"), "import sys\nopen('out.txt', 'w').write('made by the skill: ' + ' '.join(sys.argv[1:]))\nprint('ran')\n").unwrap();
    let (skills, diags) = wb_skills::SkillSet::discover(&[wb_skills::SkillRoot::new(
        wb_skills::SkillSource::Project,
        rig.ws.project.root.join(".present/skills"),
    )]);
    assert!(diags.is_empty(), "{diags:?}");
    let scripts = vec![
        vec![
            call("1", "skill_load", json!({"name": "board-deck"})),
            call(
                "2",
                "skill_run_script",
                json!({"name": "board-deck", "script": "hello.py", "args": ["x"]}),
            ),
            stop_tools(),
        ],
        vec![text("ok")],
    ];
    let approver = Recording::new(vec![]);
    let policy = auto().with_class("skills_scripts", Level::Ask);
    let (mut agent, model) = agent(&rig, scripts, policy, approver.clone(), Mode::Build);
    agent.state.skills = Some(skills);
    agent.run_turn("Use the board deck rules").await.unwrap();
    let system = model.recorded()[0].system.clone().unwrap();
    assert!(system.contains("board-deck") && system.contains("House rules for board decks"));
    assert!(
        !system.contains("SECRET-BODY-MARKER"),
        "skill bodies are loaded on demand, not up front"
    );
    let results = tool_results(&model, 1);
    assert!(results[0].1["instructions"]
        .as_str()
        .unwrap()
        .contains("SECRET-BODY-MARKER"));
    assert!(results[1].0, "{}", results[1].1);
    assert_eq!(approver.requests().len(), 1);
    assert_eq!(approver.requests()[0].kind, "skill-script");
    let produced = rig
        .ws
        .project
        .root
        .join(results[1].1["files"][0].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(produced).unwrap(),
        "made by the skill: x"
    );
    // With the class denied the script does not run and the user is not asked.
    let (mut denied, model2) = {
        let model = Arc::new(ScriptedModelClient::new(vec![
            vec![
                call(
                    "1",
                    "skill_run_script",
                    json!({"name": "board-deck", "script": "hello.py"}),
                ),
                stop_tools(),
            ],
            vec![text("ok")],
        ]));
        let config = AgentConfig::new(Mode::Build, "scripted/model", caps(false), 20);
        (
            Agent::new(
                model.clone(),
                rig.ws.clone(),
                auto().with_class("skills_scripts", Level::Deny),
                approver.clone(),
                config,
                rig.store.clone(),
                "c-3",
            ),
            model,
        )
    };
    denied.run_turn("again").await.unwrap();
    assert!(!tool_results(&model2, 1)[0].0);
    assert_eq!(approver.requests().len(), 1);
}

#[tokio::test]
async fn max_steps_bounds_the_loop_and_model_errors_surface() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    let looping: Vec<Vec<ModelEvent>> = (0..10)
        .map(|i| {
            vec![
                call(&format!("{i}"), "deck_outline", json!({})),
                stop_tools(),
            ]
        })
        .collect();
    let model = Arc::new(ScriptedModelClient::new(looping));
    let config = AgentConfig::new(Mode::Build, "scripted/model", caps(false), 4);
    let mut agent = Agent::new(
        model.clone(),
        rig.ws.clone(),
        auto(),
        Arc::new(AutoApprover),
        config,
        rig.store.clone(),
        "c-1",
    );
    let summary = agent.run_turn("loop").await.unwrap();
    assert!(summary.stopped_by_max_steps && summary.steps == 4 && model.calls() == 4);
    let failing = Arc::new(ScriptedModelClient::new(vec![vec![ModelEvent::Stop {
        reason: StopReason::Error,
        message: Some("gateway timeout".into()),
    }]]));
    let config = AgentConfig::new(Mode::Build, "scripted/model", caps(false), 4);
    let mut agent = Agent::new(
        failing,
        rig.ws.clone(),
        auto(),
        Arc::new(AutoApprover),
        config,
        rig.store.clone(),
        "c-err",
    );
    assert!(agent
        .run_turn("x")
        .await
        .unwrap_err()
        .to_string()
        .contains("gateway timeout"));
}

#[tokio::test]
async fn a_conversation_resumes_from_its_transcript() {
    let Some(rig) = rig("plain").await else {
        return;
    };
    seed(&rig, "s-aaaa", "Slide A", false);
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            text("Reading the deck."),
            call("t1", "deck_outline", json!({})),
            stop_tools(),
        ],
        vec![text("The deck has one slide.")],
    ]));
    let config = AgentConfig::new(Mode::Build, "scripted/model", caps(false), 6);
    let mut agent = Agent::new(
        model.clone(),
        rig.ws.clone(),
        auto(),
        Arc::new(AutoApprover),
        config.clone(),
        rig.store.clone(),
        "c-tp",
    );
    let summary = agent.run_turn("What is in the deck?").await.unwrap();
    assert_eq!(summary.tool_calls, 1);
    assert!(!model.recorded()[0].tools.is_empty());
    let results = tool_results(&model, 1);
    assert!(results[0].0 && results[0].1.to_string().contains("s-aaaa"));
    // A new agent on the same conversation rebuilds its history from the transcript.
    let model2 = Arc::new(ScriptedModelClient::new(vec![vec![text(
        "Still one slide.",
    )]]));
    let mut resumed = Agent::new(
        model2.clone(),
        rig.ws.clone(),
        auto(),
        Arc::new(AutoApprover),
        config,
        rig.store.clone(),
        "c-tp",
    );
    resumed.resume().unwrap();
    resumed.run_turn("And now?").await.unwrap();
    let history = &model2.recorded()[0].messages;
    assert!(history.len() >= 4 && history[0].text().contains("What is in the deck?"));
    assert!(
        history[1]
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::ToolCall { name, .. } if name == "deck_outline")),
        "the replayed history carries native tool calls"
    );
    assert!(history.last().unwrap().text().contains("And now?"));
}
