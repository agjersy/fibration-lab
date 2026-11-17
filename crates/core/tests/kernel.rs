//! The Rust core talking to the real Python kernel over stdio.

use present_core::kernel::{resolve_runtime_python, KernelClient, KernelLaunch};
use present_core::project::Project;
use present_protocol::deck::DeckMode;
use serde_json::json;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

async fn connect(project: &Path, home: &Path) -> Option<KernelClient> {
    let python = resolve_runtime_python(home, None)?;
    let launch = KernelLaunch {
        python,
        project: project.to_path_buf(),
        home: home.to_path_buf(),
        engine: "none".into(),
        sandbox: "auto".into(),
        env: vec![(
            "PRESENT_FONT_DIRS".into(),
            repo().join("vendor/fonts").to_string_lossy().into_owned(),
        )],
    };
    Some(KernelClient::connect(&launch).await.expect("kernel starts"))
}

#[tokio::test]
async fn hello_inspect_render_check_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let template = repo().join("evals/templates/corp/corp.potx");
    let project = Project::create(
        &tmp.path().join("p"),
        "Kernel test",
        &template,
        DeckMode::Structured,
    )
    .unwrap();
    let Some(kernel) = connect(&project.root, &tmp.path().join("home")).await else {
        eprintln!("skipped: no kernel interpreter (run `uv sync` in python/kernel)");
        return;
    };
    assert_eq!(kernel.hello()["protocol_version"], 1);
    assert!(kernel.hello()["sandbox"]["kind"].is_string());

    let desc = kernel
        .call_value("template.inspect", json!({"path": "template/corp.potx"}))
        .await
        .unwrap();
    assert_eq!(desc["theme"]["fonts"]["minor"], "Carlito");
    // The typed description parses the kernel's output.
    let typed: present_protocol::template::TemplateDescription =
        serde_json::from_value(desc).unwrap();
    assert!(typed.masters[0]
        .layouts
        .iter()
        .any(|l| l.name == "Layout 7"));

    let store = project.deck();
    let slide: present_protocol::deck::Slide = serde_json::from_value(json!({
        "id": "s-k001", "kind": "layout", "layout": "Title and Content", "elements": [],
        "placeholders": {
            "title": {"type": "text", "id": "e-title", "text": {"paragraphs": [{"runs": [{"t": "From Rust"}]}]}},
            "body": {"type": "text", "id": "e-body", "text": {"paragraphs": [{"runs": [{"t": "Rendered by the kernel"}]}]}}
        }
    }))
    .unwrap();
    store.create_slide(&slide, &Default::default()).unwrap();
    let out = kernel
        .call_value(
            "deck.render",
            json!({"out": "exports/deck.pptx", "snapshot_id": "h-test000001"}),
        )
        .await
        .unwrap();
    assert_eq!(out["element_map"].as_array().unwrap().len(), 2);
    assert!(project.root.join("exports/deck.pptx").is_file());
    let diags = kernel
        .call_value(
            "check.run",
            json!({"pptx": "exports/deck.pptx", "engine": false}),
        )
        .await
        .unwrap();
    let rules: Vec<&str> = diags
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"tpl.mandatory"), "{rules:?}");

    let err = kernel
        .call_value("template.inspect", json!({"path": "../escape.pptx"}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(2001));
    let err = kernel
        .call_value("no.such.method", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(-32601));
    kernel.shutdown().await;
}
