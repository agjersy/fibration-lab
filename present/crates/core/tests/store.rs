//! The deck store, references, and the reference index.

use analyst_deck::deck_store::{DeckError, Position};
use analyst_deck::index::{ChunkRow, Index, RefRow};
use analyst_deck::project::Project;
use analyst_deck::refs::{self, Added};
use analyst_deck_protocol::deck::{DeckMode, Slide};
use std::path::Path;

fn template(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("t.pptx");
    std::fs::write(&path, b"not a real template; the store never opens it").unwrap();
    path
}

fn project(dir: &Path) -> Project {
    Project::create(
        &dir.join("proj"),
        "Test deck",
        &template(dir),
        DeckMode::Structured,
    )
    .unwrap()
}

fn slide(id: &str, title: &str) -> Slide {
    serde_json::from_value(serde_json::json!({
        "id": id, "kind": "layout", "layout": "Title and Content",
        "placeholders": {"title": {"type": "text", "id": format!("e-{id}"), "text": {"paragraphs": [{"runs": [{"t": title}]}]}}},
        "elements": []
    }))
    .unwrap()
}

#[test]
fn project_skeleton_and_slide_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let p = project(tmp.path());
    assert!(
        p.root.join("template/t.pptx").is_file() && p.root.join(".present/project.toml").is_file()
    );
    assert!(Project::create(
        &p.root,
        "again",
        &template(tmp.path()),
        DeckMode::Structured
    )
    .is_err());
    let store = p.deck();
    let a = slide("s-aaaa", "A");
    let hash = store.create_slide(&a, &Position::default()).unwrap();
    store
        .create_slide(&slide("s-bbbb", "B"), &Position::default())
        .unwrap();
    store
        .create_slide(
            &slide("s-cccc", "C"),
            &Position {
                after: Some(a.id.clone()),
                section: None,
            },
        )
        .unwrap();
    let order: Vec<String> = store
        .read_deck()
        .unwrap()
        .slide_ids()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(order, ["s-aaaa", "s-cccc", "s-bbbb"]);
    // Conflict detection by content hash.
    let changed = slide("s-aaaa", "A changed");
    let new_hash = store.update_slide(&changed, Some(&hash)).unwrap();
    assert!(matches!(
        store.update_slide(&changed, Some(&hash)),
        Err(DeckError::Conflict(_))
    ));
    assert_eq!(store.slide_hash(&a.id).unwrap(), new_hash);
    store
        .move_slide(
            &a.id,
            &Position {
                after: Some("s-bbbb".parse().unwrap()),
                section: None,
            },
        )
        .unwrap();
    store.delete_slide(&"s-cccc".parse().unwrap()).unwrap();
    let order: Vec<String> = store
        .read_deck()
        .unwrap()
        .slide_ids()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(order, ["s-bbbb", "s-aaaa"]);
    assert!(!store.slide_path(&"s-cccc".parse().unwrap()).exists());
    assert!(matches!(
        store.delete_slide(&"s-zzzz".parse().unwrap()),
        Err(DeckError::NotFound(_))
    ));
    assert!(Project::open(tmp.path()).is_err());
}

#[test]
fn references_are_stored_once_and_versioned() {
    let tmp = tempfile::tempdir().unwrap();
    let p = project(tmp.path());
    let src = tmp.path().join("Q3 report.md");
    std::fs::write(&src, "# Q3\n\nMargin 18.2%\n").unwrap();
    let Added::New(first) = refs::add_file(&p.root, &src, None).unwrap() else {
        panic!("expected a new reference")
    };
    assert_eq!(first.title, "Q3 report");
    assert!(p.root.join(&first.file).is_file());
    assert!(matches!(
        refs::add_file(&p.root, &src, None).unwrap(),
        Added::Unchanged(_)
    ));
    let copy = tmp.path().join("copy.md");
    std::fs::copy(&src, &copy).unwrap();
    assert!(
        matches!(
            refs::add_file(&p.root, &copy, None).unwrap(),
            Added::Unchanged(_)
        ),
        "same bytes, different path"
    );
    let stored = std::fs::read_dir(p.root.join("references"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "md")
        })
        .count();
    assert_eq!(stored, 1);
    // A changed file becomes a new version; the old version stays for existing citations.
    std::fs::write(&src, "# Q3\n\nMargin 18.4% (restated)\n").unwrap();
    let Added::NewVersion(second) = refs::add_file(&p.root, &src, None).unwrap() else {
        panic!("expected a new version")
    };
    assert_eq!(second.id, first.id);
    assert_eq!(second.previous_versions, vec![first.version.clone()]);
    assert!(p.root.join(&first.file).is_file() && p.root.join(&second.file).is_file());
    assert_eq!(refs::load_manifest(&p.root).unwrap().refs.len(), 1);
    assert!(refs::add_file(&p.root, &tmp.path().join("t.pptx"), None).is_ok());
    std::fs::write(tmp.path().join("x.exe"), "x").unwrap();
    assert!(refs::add_file(&p.root, &tmp.path().join("x.exe"), None).is_err());
    refs::remove(&p.root, &first.id).unwrap();
    assert!(!p.root.join(&first.file).exists() && !p.root.join(&second.file).exists());
}

fn fake_embedding(text: &str) -> Vec<f32> {
    // A bag-of-topic-words vector: enough to test fusion without a model.
    let topics = [
        "margin",
        "revenue",
        "headcount",
        "pipeline",
        "risk",
        "cash",
        "pricing",
        "churn",
    ];
    let lower = text.to_lowercase();
    topics
        .iter()
        .map(|t| lower.matches(t).count() as f32 + 0.01)
        .collect()
}

#[test]
fn hybrid_search_finds_expected_chunks() {
    let mut index = Index::in_memory().unwrap();
    let docs: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "r-report",
            "Q3 report",
            vec![
                "Gross margin rose to 18.2 percent on pricing actions in EMEA.",
                "Revenue reached 1,234.5 million with strong enterprise demand.",
                "Headcount fell three percent after the reorganisation.",
                "Operating cash flow improved to 0.9 billion.",
            ],
        ),
        (
            "r-pipeline",
            "Pipeline workbook",
            vec![
                "Pipeline coverage stands at 3.1x for the next quarter.",
                "Churn among mid-market accounts increased slightly.",
                "Pricing changes were rolled out to all new contracts.",
            ],
        ),
        (
            "r-risks",
            "Risk register",
            vec![
                "Key risk: supplier concentration in two regions.",
                "Currency risk is hedged for the next twelve months.",
                "Risk of delayed product launch remains moderate.",
                "Regulatory risk is low but monitored.",
                "Cyber risk assessment completed in September.",
            ],
        ),
    ];
    for (id, title, chunks) in &docs {
        index
            .upsert_ref(&RefRow {
                id: id.to_string(),
                version: "aaaaaaaaaaaa".into(),
                kind: "markdown".into(),
                title: title.to_string(),
                file: format!("references/{id}.md"),
                outline_json: "[]".into(),
                status: "ready".into(),
            })
            .unwrap();
        let rows: Vec<ChunkRow> = chunks
            .iter()
            .enumerate()
            .map(|(i, t)| ChunkRow {
                loc: format!("c{}-{}", i * 100, i * 100 + 99),
                heading_path: vec![title.to_string()],
                text: t.to_string(),
            })
            .collect();
        let ids = index.replace_chunks(id, "aaaaaaaaaaaa", &rows).unwrap();
        let vectors: Vec<Vec<f32>> = chunks.iter().map(|t| fake_embedding(t)).collect();
        index.set_embeddings(&ids, &vectors).unwrap();
    }
    assert_eq!(index.chunk_count().unwrap(), 12);
    let cases = [
        ("gross margin", "r-report", "c0-99"),
        ("how much revenue", "r-report", "c100-199"),
        ("headcount reduction", "r-report", "c200-299"),
        ("cash flow", "r-report", "c300-399"),
        ("pipeline coverage", "r-pipeline", "c0-99"),
        ("customer churn", "r-pipeline", "c100-199"),
        ("supplier concentration", "r-risks", "c0-99"),
        ("currency hedging", "r-risks", "c100-199"),
        ("product launch delay", "r-risks", "c200-299"),
        ("cyber assessment", "r-risks", "c400-499"),
    ];
    for (query, want_ref, want_loc) in cases {
        let hits = index
            .search(query, Some(&fake_embedding(query)), None, 8)
            .unwrap();
        let top: Vec<(String, String)> = hits
            .iter()
            .take(3)
            .map(|h| (h.ref_id.clone(), h.loc.clone()))
            .collect();
        assert!(
            top.contains(&(want_ref.to_string(), want_loc.to_string())),
            "{query}: {top:?}"
        );
    }
    // One long document cannot crowd out the rest: at most three hits per reference.
    let hits = index
        .search("risk", Some(&fake_embedding("risk")), None, 8)
        .unwrap();
    assert_eq!(hits.iter().filter(|h| h.ref_id == "r-risks").count(), 3);
    // ...unless the search is narrowed to that reference.
    let only = index
        .search("risk", None, Some(&["r-risks".to_string()]), 8)
        .unwrap();
    assert_eq!(only.len(), 5);
    assert_eq!(
        only[0].cite(),
        format!("ref:r-risks@aaaaaaaaaaaa#{}", only[0].loc)
    );
    // Lexical search alone still works (no embedding model available), and odd input is safe.
    assert_eq!(index.search("pricing", None, None, 8).unwrap().len(), 2);
    assert!(index
        .search("\"(( AND NOT", None, None, 8)
        .unwrap()
        .is_empty());
    // Replacing a reference's chunks removes the old ones from both indexes.
    index
        .replace_chunks(
            "r-risks",
            "bbbbbbbbbbbb",
            &[ChunkRow {
                loc: "c0-9".into(),
                heading_path: vec![],
                text: "Nothing notable.".into(),
            }],
        )
        .unwrap();
    assert!(index
        .search("supplier concentration", None, None, 8)
        .unwrap()
        .is_empty());
    assert_eq!(index.unembedded(10).unwrap().len(), 1);
    index.remove_ref("r-risks").unwrap();
    assert_eq!(index.refs().unwrap().len(), 2);
}
