//! End to end through the workspace: references in, search, render, check, export.

use analyst_deck::deck_store::Position;
use analyst_deck::project::Project;
use analyst_deck::workspace::{OpenOptions, Workspace};
use analyst_deck_protocol::deck::DeckMode;
use serde_json::json;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[tokio::test]
async fn references_search_render_check_export() {
    let tmp = tempfile::tempdir().unwrap();
    let project = Project::create(
        &tmp.path().join("p"),
        "Workspace test",
        &repo().join("evals/templates/plain/plain.pptx"),
        DeckMode::Structured,
    )
    .unwrap();
    let mut options = OpenOptions::new(
        &tmp.path().join("home"),
        &tmp.path().join("session/index.sqlite"),
    );
    options.engine = "none".into();
    options.kernel_env = vec![(
        "PRESENT_FONT_DIRS".into(),
        repo().join("vendor/fonts").to_string_lossy().into_owned(),
    )];
    let ws = match Workspace::open(&project.root, &options).await {
        Ok(ws) => ws,
        Err(e) if e.to_string().contains("no deck kernel interpreter") => {
            eprintln!("skipped: {e}");
            return;
        }
        Err(e) => panic!("{e:?}"),
    };
    let fixtures = repo().join("python/kernel/tests/fixtures/refs");
    let (report, _) = ws
        .add_reference(&fixtures.join("report.pdf"))
        .await
        .unwrap();
    let (mail, _) = ws.add_reference(&fixtures.join("mail.eml")).await.unwrap();
    ws.add_reference(&fixtures.join("figures.xlsx"))
        .await
        .unwrap();
    let rows = ws.reference_rows().unwrap();
    assert!(
        rows.len() >= 4,
        "the email attachment is a reference of its own: {}",
        rows.len()
    );
    let manifest = analyst_deck::refs::load_manifest(&project.root).unwrap();
    assert!(manifest
        .refs
        .iter()
        .any(|r| r.parent == mail.id.to_string()));
    // Adding the same file again changes nothing.
    let (again, _) = ws
        .add_reference(&fixtures.join("report.pdf"))
        .await
        .unwrap();
    assert_eq!(again.id, report.id);
    assert_eq!(ws.reference_rows().unwrap().len(), rows.len());

    let hits = ws.search("margin", None, 8).await.unwrap();
    assert!(!hits.is_empty());
    let cite = hits[0].cite();
    let passage = ws
        .kernel
        .call_value("refs.read", json!({"cite": cite}))
        .await
        .unwrap();
    assert!(
        passage["text"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("margin"),
        "{passage}"
    );

    let slide: analyst_deck_protocol::deck::Slide = serde_json::from_value(json!({
        "id": "s-w001", "kind": "layout", "layout": "Title and Content", "elements": [],
        "placeholders": {
            "title": {"type": "text", "id": "e-t1", "text": {"paragraphs": [{"runs": [{"t": "Sourced slide"}]}]}},
            "body": {"type": "text", "id": "e-b1", "text": {"paragraphs": [{"runs": [{"t": "A point from the report", "cite": [cite]}]}]}}
        }
    }))
    .unwrap();
    ws.deck()
        .create_slide(&slide, &Position::default())
        .unwrap();
    let exported = ws.export("pptx", None, "").await.unwrap();
    assert_eq!(exported.path, "exports/workspace-test.pptx");
    assert!(project.root.join(&exported.path).is_file());
    assert_eq!(
        exported.errors, 0,
        "the citation resolves through the manifest"
    );
    let md = ws.export("md", None, "").await.unwrap();
    let outline = std::fs::read_to_string(project.root.join(md.path)).unwrap();
    assert!(outline.contains("Sourced slide") && outline.contains(&cite));
    // The index is per-user state outside the deck folder; a second session that opens
    // the same deck finds the references and indexes them again.
    assert!(tmp.path().join("session/index.sqlite").is_file());
    assert!(!project.root.join(".present/cache/index.sqlite").exists());
    assert!(!project.root.join(".present/history").exists());
    assert!(ws.export("odp", None, "").await.is_err());
    ws.close().await;
    let mut other = options.clone();
    other.index_path = tmp.path().join("session2/index.sqlite");
    let again = Workspace::open(&project.root, &other).await.unwrap();
    assert_eq!(again.reference_rows().unwrap().len(), rows.len());
    assert!(!again.search("revenue", None, 3).await.unwrap().is_empty());
    again.close().await;
}
