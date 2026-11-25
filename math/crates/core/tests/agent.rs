//! The agent loop with a scripted model against the real engine and runner. Skips when the
//! engine environment is absent.

use std::path::Path;
use std::sync::Arc;

use math_core::agent::{Agent, AgentConfig, AgentEvent, ClaimStatus, Mode, TurnEnd};
use math_core::engine::{engine_available, EngineConfig};
use math_core::replay::ReplayOptions;
use math_core::{SceneSession, SessionConfig};
use serde_json::{json, Value};
use wb_providers::model::{ContentPart, ModelEvent, StopReason};
use wb_providers::ScriptedModelClient;

async fn open(dir: &Path) -> Option<SceneSession> {
    let engine = EngineConfig::from_env();
    if !engine_available(&engine.python) {
        eprintln!(
            "skipping: no engine environment at {}",
            engine.python.display()
        );
        return None;
    }
    let config = SessionConfig {
        engine,
        replay: ReplayOptions::default(),
        socket: dir.join("s/attach.sock"),
        call_log: None,
    };
    Some(
        SceneSession::open(&dir.join("scene.py"), config)
            .await
            .expect("open"),
    )
}

fn call(id: &str, name: &str, arguments: Value) -> ModelEvent {
    ModelEvent::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    }
}

fn step(events: Vec<ModelEvent>) -> Vec<ModelEvent> {
    let mut events = events;
    events.push(ModelEvent::Stop {
        reason: StopReason::ToolCalls,
        message: None,
    });
    events
}

fn say(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Text { delta: text.into() },
        ModelEvent::Stop {
            reason: StopReason::End,
            message: None,
        },
    ]
}

/// The tool results the model was sent in request `n` (0-based), as (is_error, content).
fn tool_results(client: &ScriptedModelClient, n: usize) -> Vec<(bool, String)> {
    client.recorded()[n]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::ToolResult {
                content, is_error, ..
            } => Some((*is_error, content.clone())),
            _ => None,
        })
        .collect()
}

const GOOD_CELL: &str = "from fibration_math import sym, plot\n\
x = sym.symbols(\"x\")\n\
plot.explicit(sym.define(\"sin(x)/x\"), x=(-10, 10), id=\"sinc\")\n";

#[tokio::test]
async fn a_turn_grounds_claims_and_repairs_a_failed_cell() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut session) = open(dir.path()).await else {
        return;
    };
    let client = Arc::new(ScriptedModelClient::new(vec![
        step(vec![call(
            "t1",
            "math_query",
            json!({"calls": [
                {"fn": "limit", "args": {"expr": "sin(x)/x", "var": "x", "at": 0}},
                {"fn": "roots_in", "args": {"expr": "cos(x) - x", "var": "x", "within": [0, 2]}},
                {"fn": "numeric", "args": {"expr": "pi"}},
                {"fn": "plot.point", "args": {"at": [0, 0]}},
            ]}),
        )]),
        step(vec![
            call(
                "t2",
                "claim_make",
                json!({"text": "sin(x)/x tends to 1 at 0", "support": ["q1"]}),
            ),
            call(
                "t3",
                "claim_make",
                json!({"text": "pi is about 3.14159", "support": ["q1", "q3"]}),
            ),
            call(
                "t4",
                "claim_make",
                json!({"text": "cos(x) = x has exactly one root", "support": ["q2"]}),
            ),
            call(
                "t5",
                "claim_make",
                json!({"text": "it looks smooth", "support": []}),
            ),
            call(
                "t6",
                "claim_make",
                json!({"text": "made up", "support": ["q99"]}),
            ),
            call(
                "t7",
                "claim_make",
                json!({"text": "from a failed call", "support": ["q4"]}),
            ),
            call(
                "t8",
                "scene_propose_cell",
                json!({"code": "plot.explicit(nope, x=(-1, 1))\n", "caption": "Broken"}),
            ),
        ]),
        step(vec![call(
            "t9",
            "scene_propose_cell",
            json!({"code": GOOD_CELL, "caption": "Graph sin(x)/x"}),
        )]),
        say("Graphed; the hole at 0 has limit 1."),
    ]));
    let transcript = dir.path().join("t/events.jsonl");
    let mut agent = Agent::new(
        client.clone(),
        AgentConfig {
            transcript: Some(transcript.clone()),
            ..AgentConfig::default()
        },
    );
    let report = agent
        .turn(&mut session, "does sin(x)/x blow up at 0? graph it")
        .await
        .unwrap();

    assert_eq!(report.end, TurnEnd::Done);
    assert_eq!(report.steps, 4);
    assert_eq!(report.text, "Graphed; the hole at 0 has limit 1.");

    // Badges come from the cited results, not from the model.
    let status: Vec<ClaimStatus> = report.claims.iter().map(|c| c.status).collect();
    assert_eq!(
        status,
        [
            ClaimStatus::Proven,
            ClaimStatus::Checked,
            ClaimStatus::Checked,
            ClaimStatus::Unverified
        ]
    );
    // The numeric root search supports "checked", and says what it leaves open.
    assert!(report.claims[2]
        .note
        .as_ref()
        .unwrap()
        .contains("could not confirm the list is complete"));

    // The scene-changing call inside a query was refused, and the two bad citations were errors.
    let q = tool_results(&client, 1);
    let queried: Value = serde_json::from_str(&q[0].1).unwrap();
    let q4 = &queried["results"][3];
    assert_eq!(
        (q4["id"].as_str(), q4["ok"].as_bool()),
        (Some("q4"), Some(false))
    );
    assert!(q4["result"]["error"]
        .as_str()
        .unwrap()
        .contains("pure operations"));
    // Engine objects come back with their text, so the model can read them.
    assert_eq!(queried["results"][0]["result"]["value"]["text"], "1");
    let r = tool_results(&client, 2);
    assert!(r[4].0 && r[4].1.contains("q99"));
    assert!(r[5].0 && r[5].1.contains("failed"));
    // The broken cell came back as an error with its type and line, and nothing changed.
    assert!(r[6].0);
    let refused: Value = serde_json::from_str(&r[6].1).unwrap();
    assert_eq!(refused["error"]["type"], "NameError");
    assert_eq!(refused["error"]["line"], 1);
    assert_eq!(refused["attempts_left"], 2);

    // The repaired cell was kept.
    assert_eq!(report.cells, ["c1"]);
    assert_eq!(report.refused, 1);
    assert_eq!(session.state()["objects"][0]["id"], "sinc");
    assert!(std::fs::read_to_string(session.path())
        .unwrap()
        .contains("# %% [c1] Graph sin(x)/x"));

    // The first request carried the scene and the system prompt; every event was written.
    let first = &client.recorded()[0];
    assert!(first
        .system
        .as_ref()
        .unwrap()
        .contains("Never state a mathematical fact"));
    assert!(first.messages[0].text().contains("<scene>"));
    let lines = std::fs::read_to_string(&transcript).unwrap();
    assert_eq!(lines.lines().count(), report.events.len());
    assert!(matches!(
        report.events.last(),
        Some(AgentEvent::Stop { .. })
    ));

    // A second turn sees the scene as it now is.
    let client2 = Arc::new(ScriptedModelClient::new(vec![say("ok")]));
    let mut agent2 = Agent::new(client2.clone(), AgentConfig::default());
    agent2
        .turn(&mut session, "what is on the canvas?")
        .await
        .unwrap();
    assert!(client2.recorded()[0].messages[0].text().contains("sinc"));
    session.close().await.unwrap();
}

#[tokio::test]
async fn proposals_stop_after_the_repair_limit_and_plan_mode_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut session) = open(dir.path()).await else {
        return;
    };
    let broken = |n: u32| {
        step(vec![call(
            &format!("t{n}"),
            "scene_propose_cell",
            json!({"code": format!("undefined_{n}\n"), "caption": "Try"}),
        )])
    };
    let client = Arc::new(ScriptedModelClient::new(vec![
        broken(1),
        broken(2),
        broken(3),
        step(vec![call(
            "t4",
            "scene_propose_cell",
            json!({"code": GOOD_CELL, "caption": "Too late"}),
        )]),
        say("I could not do that."),
    ]));
    let mut agent = Agent::new(client.clone(), AgentConfig::default());
    let report = agent.turn(&mut session, "do the impossible").await.unwrap();
    assert_eq!(report.refused, 3);
    assert!(report.cells.is_empty());
    let third: Value = serde_json::from_str(&tool_results(&client, 3)[0].1).unwrap();
    assert_eq!(third["attempts_left"], 0);
    // The fourth proposal is not even tried, although it would have run.
    let last = &tool_results(&client, 4)[0];
    assert!(
        last.0 && last.1.contains("no further changes"),
        "{}",
        last.1
    );
    assert!(session.state()["objects"].as_array().unwrap().is_empty());
    assert_eq!(session.notebook().cells.len(), 0);

    // Plan mode: the tool is not offered, and a call to it anyway is refused.
    let client = Arc::new(ScriptedModelClient::new(vec![
        step(vec![call(
            "p1",
            "scene_propose_cell",
            json!({"code": GOOD_CELL, "caption": "Sneak"}),
        )]),
        say("Understood."),
    ]));
    let mut agent = Agent::new(
        client.clone(),
        AgentConfig {
            mode: Mode::Plan,
            ..AgentConfig::default()
        },
    );
    let report = agent.turn(&mut session, "graph it").await.unwrap();
    assert!(report.cells.is_empty());
    assert!(!client.recorded()[0]
        .tools
        .iter()
        .any(|t| t.name == "scene_propose_cell"));
    assert!(tool_results(&client, 1)[0].1.contains("plan mode"));
    assert!(session.state()["objects"].as_array().unwrap().is_empty());

    // A model that never stops calling tools ends at the step limit.
    let looping: Vec<_> = (0..5)
        .map(|i| step(vec![call(&format!("l{i}"), "scene_read", json!({}))]))
        .collect();
    let client = Arc::new(ScriptedModelClient::new(looping));
    let mut agent = Agent::new(
        client,
        AgentConfig {
            max_steps: 3,
            ..AgentConfig::default()
        },
    );
    assert_eq!(
        agent.turn(&mut session, "loop").await.unwrap().end,
        TurnEnd::StepLimit
    );
    session.close().await.unwrap();
}

const FAMILY: &str = "from fibration_math import sym, plot, param\n\
x = sym.symbols(\"x\")\n\
a = param.declare(a=(1.0, -3, 3))\n\
plot.explicit(sym.define(\"x**2 - a\"), x=(-3, 3), id=\"curve\")\n";

#[tokio::test]
async fn a_claim_taken_at_a_value_is_checked_again_when_the_value_moves() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut session) = open(dir.path()).await else {
        return;
    };
    assert!(
        session
            .apply(math_core::CellEdit::Append {
                code: FAMILY.into(),
                caption: "Family".into()
            })
            .await
            .unwrap()
            .ok
    );

    // Handles are named by content, so a first agent can find the handle the second will get.
    let subs = json!({"fn": "subs", "args": {"expr": "x**2 - a", "values": {"a": 1}}});
    let scout = Arc::new(ScriptedModelClient::new(vec![
        step(vec![call(
            "s1",
            "math_query",
            json!({"calls": [subs.clone()]}),
        )]),
        say("ok"),
    ]));
    let found = Agent::new(scout, AgentConfig::default())
        .turn(&mut session, "look")
        .await
        .unwrap();
    let handle = found
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Evidence(ev) => ev.result["$h"].as_u64(),
            _ => None,
        })
        .expect("the substituted expression's handle");

    let client = Arc::new(ScriptedModelClient::new(vec![
        step(vec![call(
            "t1",
            "math_query",
            json!({"calls": [
                subs,                                                                        // q1: at a = 1
                {"fn": "roots_in", "args": {"expr": {"$h": handle}, "var": "x", "within": [-3, 3]}}, // q2: from q1
                {"fn": "evaluate", "args": {"expr": "x - 0*a", "at": {"a": 1, "x": 2}}},     // q3: same at any a
                {"fn": "diff", "args": {"expr": "x**2 - a", "var": "x"}},                     // q4: for every a
            ]}),
        )]),
        step(vec![
            call(
                "t2",
                "claim_make",
                json!({"text": "the curve crosses zero at x = -1 and x = 1", "support": ["q2"]}),
            ),
            call(
                "t3",
                "claim_make",
                json!({"text": "the value at x = 2 is 2", "support": ["q3"]}),
            ),
            call(
                "t4",
                "claim_make",
                json!({"text": "the slope is 2x", "support": ["q4"]}),
            ),
        ]),
        say("Done."),
        say("Nothing new."),
    ]));
    let mut agent = Agent::new(client, AgentConfig::default());
    let report = agent
        .turn(&mut session, "where does it cross zero?")
        .await
        .unwrap();
    let at_one = std::collections::BTreeMap::from([("a".to_string(), 1.0)]);
    assert_eq!(report.claims[0].at, at_one);
    assert_eq!(report.claims[0].status, ClaimStatus::Proven);
    assert!(report.claims[0]
        .note
        .as_deref()
        .unwrap()
        .contains("holds at a = 1"));
    assert_eq!(report.claims[1].at, at_one);
    assert!(report.claims[2].at.is_empty() && report.claims[2].note.is_none());
    // Nothing moved: nothing to check again.
    assert!(agent.recheck_claims(&session).await.is_empty());

    // The slider moves. The roots are now at ±sqrt(2): the first claim is no longer earned.
    assert!(session.commit_param("a", 2.0).await.unwrap().ok);
    let updates = agent.recheck_claims(&session).await;
    assert_eq!(updates.iter().map(|u| u.index).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(updates[0].claim.status, ClaimStatus::Unverified);
    let note = updates[0].claim.note.as_deref().unwrap();
    assert!(
        note.contains("established at a = 1") && note.contains("at a = 2") && note.contains("q2"),
        "{note}"
    );
    // The second claim's evidence comes out the same at a = 2, so it keeps its badge.
    assert_eq!(updates[1].claim.status, ClaimStatus::Proven);
    assert!(updates[1]
        .claim
        .note
        .as_deref()
        .unwrap()
        .contains("same answers at a = 2"));
    // The claim that holds for every value was not touched.
    assert_eq!(agent.claims()[2].status, ClaimStatus::Proven);
    assert!(agent.recheck_claims(&session).await.is_empty());

    // Moving back restores what was earned there.
    assert!(session.commit_param("a", 1.0).await.unwrap().ok);
    let updates = agent.recheck_claims(&session).await;
    assert_eq!(updates[0].claim.status, ClaimStatus::Proven);
    assert!(updates[0]
        .claim
        .note
        .as_deref()
        .unwrap()
        .contains("holds at a = 1"));

    // A turn settles the claims before the model speaks, and reports it.
    assert!(session.commit_param("a", 2.5).await.unwrap().ok);
    let report = agent.turn(&mut session, "and now?").await.unwrap();
    let rechecked: Vec<_> = report
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ClaimRechecked(u) => Some((u.index, u.claim.status)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rechecked,
        [(0, ClaimStatus::Unverified), (1, ClaimStatus::Proven)]
    );
    session.close().await.unwrap();
}
