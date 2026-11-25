//! The scene session against the real engine and cell runner (needs the engine environment:
//! `cd math/python/engine && uv sync`). Skips when it is absent.

use std::path::Path;

use math_core::chips::ChipKind;
use math_core::engine::{engine_available, EngineConfig};
use math_core::replay::ReplayOptions;
use math_core::{CellEdit, SceneSession, SessionConfig};

const DEFINE: &str = "from fibration_math import sym, plot, param\n\
x = sym.symbols(\"x\")\n\
a = param.declare(a=(1.0, -3, 3))\n\
f = sym.define(\"a*sin(x)\")\n\
plot.explicit(f, x=(-10, 10), id=\"curve\")\n";

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
        call_log: Some(dir.join("calls.jsonl")),
    };
    Some(
        SceneSession::open(&dir.join("scenes/demo/scene.py"), config)
            .await
            .expect("open"),
    )
}

fn object_ids(session: &SceneSession) -> Vec<String> {
    session.state()["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn edits_are_kept_only_when_they_run_and_undo_restores_them() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut s) = open(dir.path()).await else {
        return;
    };
    // A new scene file is created with just the header.
    assert_eq!(
        std::fs::read_to_string(s.path()).unwrap(),
        "# fibration-math: scene api=1\n"
    );
    assert!(!s.can_undo());

    let applied = s
        .apply(CellEdit::Append {
            code: DEFINE.into(),
            caption: "Define".into(),
        })
        .await
        .unwrap();
    assert!(applied.ok);
    assert_eq!(applied.cell.as_deref(), Some("c1"));
    assert_eq!(object_ids(&s), ["curve"]);
    let kept_hash = s.last().hash.clone();
    let kept_file = std::fs::read_to_string(s.path()).unwrap();
    assert!(kept_file.contains("# %% [c1] Define"));

    // A cell that fails is refused: file, notebook and engine scene are unchanged.
    let refused = s
        .apply(CellEdit::Append {
            code: "plot.explicit(f + undefined_name, x=(-1, 1))\n".into(),
            caption: "Broken".into(),
        })
        .await
        .unwrap();
    assert!(!refused.ok);
    let failure = refused.failure().unwrap();
    assert_eq!(failure.id, "c2");
    assert_eq!(failure.error.as_ref().unwrap().kind, "NameError");
    assert_eq!(failure.error.as_ref().unwrap().line, Some(1));
    assert_eq!(std::fs::read_to_string(s.path()).unwrap(), kept_file);
    assert_eq!(s.last().hash, kept_hash);
    assert_eq!(s.notebook().cells.len(), 1);

    // An engine error carries its classified form.
    let refused = s
        .apply(CellEdit::Append {
            code: "plot.explicit(f + sym.define(\"q\"), x=(-1, 1))\n".into(),
            caption: "Unbound".into(),
        })
        .await
        .unwrap();
    let err = refused.failure().unwrap().error.as_ref().unwrap();
    assert_eq!(err.call_error.as_ref().unwrap().kind, "bad_args");
    assert_eq!(s.last().hash, kept_hash);

    // Undo goes back to the empty scene, and the file follows.
    assert!(s.undo().await.unwrap().is_some());
    assert!(object_ids(&s).is_empty());
    assert_eq!(
        std::fs::read_to_string(s.path()).unwrap(),
        "# fibration-math: scene api=1\n"
    );
    assert!(s.undo().await.unwrap().is_none());
    s.close().await.unwrap();
}

#[tokio::test]
async fn chips_sliders_and_external_edits() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut s) = open(dir.path()).await else {
        return;
    };

    // Empty scene: only ways to start; chips need no model.
    let chips = s.chips().await.unwrap();
    assert!(chips.iter().all(|c| c.kind == ChipKind::Seed));

    s.apply(CellEdit::Append {
        code: DEFINE.into(),
        caption: "Define".into(),
    })
    .await
    .unwrap();
    let chips = s.chips().await.unwrap();
    let ids: Vec<&str> = chips.iter().map(|c| c.id.as_str()).collect();
    assert!(
        ids.contains(&"adjust:a") && ids.contains(&"derivative:curve"),
        "{ids:?}"
    );

    // A direct chip becomes a cell and can be undone like any edit.
    let fired = s.fire_chip("derivative:curve").await.unwrap();
    assert!(fired.ok, "{:?}", fired.failure());
    assert_eq!(object_ids(&s).len(), 2);
    let derivative = &s.state()["objects"][1];
    assert_eq!(derivative["exprs"]["f"]["text"], "a*cos(x)");
    assert_eq!(derivative["origin"], "c2");
    assert!(s.fire_chip("tangent:curve").await.is_err()); // a seed chip needs input

    // Slider releases: one cell, replaced by the next release of the same parameter.
    assert!(s.commit_param("a", 2.5).await.unwrap().ok);
    assert!(s.commit_param("a", -1.25).await.unwrap().ok);
    assert_eq!(s.notebook().cells.len(), 3);
    assert_eq!(s.state()["params"]["a"]["value"], -1.25);
    assert!(!s.commit_param("a", 99.0).await.unwrap().ok); // outside the declared range
    assert_eq!(s.state()["params"]["a"]["value"], -1.25);
    assert!(s.commit_param("zz", 1.0).await.is_err());

    // A query may not change the scene.
    let limit = s
        .query(
            "limit",
            serde_json::json!({"expr": "sin(x)/x", "var": "x", "at": 0}),
            "agent",
        )
        .await
        .unwrap();
    assert_eq!(limit["value"]["exists"]["truth"], "true");
    assert!(s
        .query("plot.point", serde_json::json!({"at": [0, 0]}), "agent")
        .await
        .is_err());
    assert!(s
        .query("no.such", serde_json::json!({}), "agent")
        .await
        .is_err());

    // An edit made in another editor is picked up before the next change, not overwritten.
    let text = std::fs::read_to_string(s.path())
        .unwrap()
        .replace("a*sin(x)", "a*cos(x)");
    std::fs::write(s.path(), text).unwrap();
    assert!(s.commit_param("a", 1.0).await.unwrap().ok);
    assert_eq!(s.state()["objects"][0]["exprs"]["f"]["text"], "a*cos(x)");

    // Every call was logged with its origin.
    let log = std::fs::read_to_string(dir.path().join("calls.jsonl")).unwrap();
    assert!(log.lines().all(|l| l.contains("\"origin\"")));
    assert!(log.contains("\"kind\":\"agent\"") && log.contains("\"cell\":\"c1\""));
    s.close().await.unwrap();
}
