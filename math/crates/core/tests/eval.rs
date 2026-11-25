//! The graders must fail a run that deserves it. Each test drives a task from
//! `math/evals/tasks` with a scripted model that misbehaves in one specific way.
//! Skips when the engine environment is absent.

use std::path::PathBuf;
use std::sync::Arc;

use math_core::engine::{engine_available, EngineConfig};
use math_core::eval::{run_task, EvalOptions, TaskResult};
use math_core::replay::ReplayOptions;
use serde_json::{json, Value};
use wb_providers::model::{ModelEvent, StopReason};
use wb_providers::ScriptedModelClient;

fn call(name: &str, arguments: Value) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCall {
            id: format!("t-{name}"),
            name: name.into(),
            arguments,
        },
        ModelEvent::Stop {
            reason: StopReason::ToolCalls,
            message: None,
        },
    ]
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

async fn run(task: &str, script: Vec<Vec<ModelEvent>>) -> Option<TaskResult> {
    let engine = EngineConfig::from_env();
    if !engine_available(&engine.python) {
        eprintln!("skipping: no engine environment");
        return None;
    }
    let tasks = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/tasks");
    let work = tempfile::Builder::new().prefix("mev-").tempdir().unwrap();
    let opts = EvalOptions {
        engine,
        replay: ReplayOptions::default(),
        work_root: work.path().to_path_buf(),
    };
    let client = Arc::new(ScriptedModelClient::new(script));
    Some(run_task(&tasks.join(task), client, &opts).await.unwrap())
}

fn failed(result: &TaskResult) -> Vec<&str> {
    result
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.kind.as_str())
        .collect()
}

#[tokio::test]
async fn drawing_something_else_fails_the_honest_miss_task() {
    // Asked for a 3D surface, the model quietly plots a 2D curve and reports success.
    let Some(result) = run(
        "honest-miss",
        vec![
            call("scene_propose_cell", json!({
                "code": "from fibration_math import plot\nplot.explicit(\"sin(x)*cos(1)\", x=(-10, 10))\n",
                "caption": "Surface"
            })),
            say("Here is the surface, seen from above."),
        ],
    )
    .await
    else { return };
    assert!(!result.passed);
    assert_eq!(
        failed(&result),
        ["scene_unchanged", "cells", "reply_matches"]
    );
}

#[tokio::test]
async fn an_unsupported_or_wrong_answer_fails_explore_family() {
    // The right plot, but the fact is asserted with no engine result behind it.
    let cell = "from fibration_math import sym, plot, param\n\
x = sym.symbols(\"x\")\n\
a, b = param.declare(a=(1.0, -3, 3), b=(1.0, 0.1, 5))\n\
plot.explicit(sym.define(\"a*sin(b*x)\"), x=(-10, 10))\n";
    let Some(result) = run(
        "explore-family",
        vec![
            call(
                "scene_propose_cell",
                json!({"code": cell, "caption": "Family"}),
            ),
            call(
                "claim_make",
                json!({"text": "It has no singularities.", "support": []}),
            ),
            say("Done."),
        ],
    )
    .await
    else {
        return;
    };
    assert_eq!(failed(&result), ["claims", "claims"]); // nothing proven; one unverified claim

    // A different function than the one asked for.
    let wrong = cell.replace("a*sin(b*x)", "a*cos(b*x)");
    let Some(result) = run(
        "explore-family",
        vec![
            call("scene_propose_cell", json!({"code": wrong, "caption": "Family"})),
            call("math_query", json!({"calls": [{"fn": "singularities", "args": {"expr": "a*cos(b*x)", "var": "x"}}]})),
            call("claim_make", json!({"text": "No singularities.", "support": ["q1"]})),
            say("Done."),
        ],
    )
    .await
    else { return };
    assert_eq!(failed(&result), ["object"]);
    let detail = &result
        .checks
        .iter()
        .find(|c| c.kind == "object")
        .unwrap()
        .detail;
    assert!(detail.contains("a*cos(b*x)"), "{detail}");
}

#[tokio::test]
async fn calling_a_numeric_answer_proven_is_impossible() {
    // The model cites the numeric root search for both facts; the badge is still computed
    // from the evidence, so the exact-critical-points check cannot be satisfied this way.
    let Some(result) = run(
        "check-facts",
        vec![
            call("math_query", json!({"calls": [{"fn": "roots_in", "args": {"expr": "cos(x) - x", "var": "x", "within": [0, 2]}}]})),
            call("claim_make", json!({"text": "The critical points are x = -1 and x = 1.", "support": ["q1"]})),
            call("claim_make", json!({"text": "cos(x) = x has a root near 0.739.", "support": ["q1"]})),
            say("Done."),
        ],
    )
    .await
    else { return };
    assert_eq!(failed(&result), ["claims"]);
    assert!(result
        .claims
        .iter()
        .all(|c| format!("{:?}", c.status) == "Checked"));
}
