//! The deck service the UI's deck commands call, against the real deck kernel. The agent
//! side (proposals, modes, scope) is tested with the one loop in `fibration-core`.

use fibration_deck::project::Project;
use fibration_deck::service::{DeckHost, DeckService};
use fibration_deck::workspace::{OpenOptions, Workspace};
use fibration_deck_protocol::deck::DeckMode;
use fibration_deck_protocol::ui::{DeckEvent, IpcErrorCode};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::Receiver;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

struct Rig {
    tmp: tempfile::TempDir,
    rt: Arc<DeckService>,
    host: Arc<CountingHost>,
}

/// Counts the checkpoints the deck service asks its session for.
#[derive(Default)]
struct CountingHost {
    labels: std::sync::Mutex<Vec<String>>,
}

impl DeckHost for CountingHost {
    fn checkpoint(&self, label: &str) -> anyhow::Result<String> {
        let mut l = self.labels.lock().unwrap();
        l.push(label.to_string());
        Ok(format!("cp-{}", l.len()))
    }
    fn restore_deck(&self, _checkpoint: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A deck folder as a session holds it (`decks/<slug>/`), opened as a deck service with its
/// per-user state in a separate directory.
async fn rig() -> Option<Rig> {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo/decks/test");
    let template = repo().join("evals/templates/plain/plain.pptx");
    Project::create(&root, "Service test", &template, DeckMode::Structured).unwrap();
    let state = tmp.path().join("session/decks/test");
    let mut options = OpenOptions::new(&tmp.path().join("home"), &state.join("index.sqlite"));
    options.engine = "none".into();
    options.kernel_env = vec![(
        "PRESENT_FONT_DIRS".into(),
        repo().join("vendor/fonts").to_string_lossy().into_owned(),
    )];
    let ws = match Workspace::open(&root, &options).await {
        Ok(ws) => ws,
        Err(e) if format!("{e:#}").contains("no deck kernel interpreter") => {
            eprintln!("skipped: {e:#}");
            return None;
        }
        Err(e) => panic!("{e:?}"),
    };
    let host = Arc::new(CountingHost::default());
    let rt = DeckService::new("test", Arc::new(ws), &state, host.clone());
    Some(Rig { tmp, rt, host })
}

async fn wait_for<T>(
    rx: &mut Receiver<DeckEvent>,
    mut pick: impl FnMut(&DeckEvent) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("timed out waiting for an event")
            .expect("event stream closed");
        if std::env::var("SERVICE_TEST_TRACE").is_ok() {
            eprintln!(
                "event: {}",
                serde_json::to_string(&event)
                    .unwrap()
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        if let Some(v) = pick(&event) {
            return v;
        }
    }
}

#[tokio::test]
async fn manual_editing_and_reopening() {
    let Some(rig) = rig().await else { return };
    let rt = &rig.rt;
    let mut rx = rt.subscribe();

    let created = rt.slide_create(None, "Title and Content").await.unwrap();
    let id = created.slide.id.to_string();
    assert_eq!(rt.deck_get().unwrap().slides.len(), 1);
    wait_for(&mut rx, |e| {
        matches!(e, DeckEvent::DeckChanged { by, .. } if by == "user").then_some(())
    })
    .await;

    // A stale hash is a conflict; the current one applies.
    let mut doc = serde_json::to_value(&created.slide).unwrap();
    doc["notes"] = json!({"paragraphs": [{"runs": [{"t": "Speaker note"}]}]});
    let edited: fibration_deck_protocol::deck::Slide = serde_json::from_value(doc).unwrap();
    let stale = rt
        .slide_update(edited.clone(), Some("sha256:0000"))
        .unwrap_err();
    assert_eq!(stale.code, IpcErrorCode::Conflict, "{stale:?}");
    let updated = rt.slide_update(edited, Some(&created.hash)).unwrap();
    assert_ne!(updated.hash, created.hash);

    let copy = rt.slide_duplicate(&id).unwrap();
    assert_ne!(copy.slide.id.to_string(), id);
    assert_eq!(rt.deck_get().unwrap().slides.len(), 2);
    rt.slide_move(copy.slide.id.as_str(), None, None).unwrap();
    let order: Vec<String> = rt
        .deck_get()
        .unwrap()
        .slides
        .iter()
        .map(|s| s.slide.id.to_string())
        .collect();
    assert_eq!(order[0], copy.slide.id.to_string());

    rt.slide_lock(&id, None, true).unwrap();
    assert!(rt
        .deck_get()
        .unwrap()
        .slides
        .iter()
        .any(|s| s.slide.locked == Some(true)));
    rt.slide_delete(copy.slide.id.as_str()).unwrap();
    assert_eq!(rt.deck_get().unwrap().slides.len(), 1);

    let checks = rt.run_checks(None).await.unwrap();
    // An empty slide is reported as untitled, and nothing else.
    assert!(
        checks.iter().all(|d| d["rule"] == "struct.titles"),
        "{checks:?}"
    );

    // Unknown layouts are refused before anything is written.
    let bad = rt.slide_create(None, "No Such Layout").await.unwrap_err();
    assert_eq!(bad.code, IpcErrorCode::Invalid);

    // The deck's info is what `deck_open` returns: no network for the kernel is reported.
    let info = rt.info().await.unwrap();
    assert_eq!(info.slug, "test");
    assert_eq!(info.slides, 1);
    assert_eq!(info.deck_mode, DeckMode::Structured);
    assert!(!info.kernel_network.is_empty());
    rt.ws.close().await;
}

#[tokio::test]
async fn references_exports_and_code_files() {
    let Some(rig) = rig().await else { return };
    let rt = &rig.rt;
    let mut rx = rt.subscribe();

    let note = rig.tmp.path().join("notes.md");
    std::fs::write(
        &note,
        "# Results\n\nRevenue rose 12 percent to 4.2 million in 2025.\n",
    )
    .unwrap();
    let task = rt.refs_add(vec![note.to_string_lossy().into_owned()]);
    let ok = wait_for(&mut rx, |e| match e {
        DeckEvent::TaskDone {
            task_id,
            ok,
            message,
        } if *task_id == task => Some((*ok, message.clone())),
        _ => None,
    })
    .await;
    assert!(ok.0, "{ok:?}");
    let refs = rt.refs_list().unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].status, "ready");
    let hits = rt.refs_search("revenue", None).await.unwrap();
    assert!(!hits.is_empty());
    let hit = serde_json::to_value(&hits[0]).unwrap();
    let cite = hit["cite"].as_str().expect("a hit carries its citation");
    let passage = rt.source_passage(cite).await.unwrap();
    assert!(
        passage.text.as_deref().unwrap_or("").contains("12 percent"),
        "{passage:?}"
    );

    // Unused references can be cleared; a missing one is not found.
    assert_eq!(
        rt.refs_remove("r-missing").unwrap_err().code,
        IpcErrorCode::NotFound
    );
    assert_eq!(rt.refs_remove_unused().unwrap().len(), 1);
    assert!(rt.refs_list().unwrap().is_empty());

    rt.slide_create(None, "Title and Content").await.unwrap();
    let task = rt.export_run("pptx".into());
    let export = wait_for(&mut rx, |e| match e {
        DeckEvent::ExportDone { export } => Some(export.clone()),
        DeckEvent::TaskDone {
            task_id,
            ok: false,
            message,
        } if *task_id == task => {
            panic!("export failed: {message:?}")
        }
        _ => None,
    })
    .await;
    assert_eq!(export.format, "pptx");
    assert!(rt.export_path(&export.id).unwrap().is_file());
    assert_eq!(rt.exports_list().len(), 1);

    // Only Python under deck/ is reachable through the code commands.
    for bad in [
        "../x.py",
        "/etc/passwd",
        "deck/../x.py",
        "refs/a.py",
        "deck/a.txt",
    ] {
        assert_eq!(
            rt.code_read(bad).unwrap_err().code,
            IpcErrorCode::Invalid,
            "{bad}"
        );
    }
    let w = rt.code_write("deck/helpers.py", "X = 1\n", None).unwrap();
    let read = rt.code_read("deck/helpers.py").unwrap();
    assert_eq!(read.hash, w.hash);
    let conflict = rt
        .code_write("deck/helpers.py", "X = 2\n", Some("sha256:0000"))
        .unwrap_err();
    assert_eq!(conflict.code, IpcErrorCode::Conflict);
    rt.code_write("deck/helpers.py", "X = 2\n", Some(&w.hash))
        .unwrap();
}

/// A 1x1 PNG.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 0x0D, 0x49, 0x44, 0x41, 0x54,
    0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0, 5, 0, 1, 0xFF, 0x89, 0x99, 0x3D, 0x1D, 0, 0,
    0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

#[tokio::test]
async fn assets_previews_and_template_replacement() {
    use base64::Engine;
    use fibration_deck_protocol::ui::ThumbKind;
    let Some(rig) = rig().await else { return };
    let rt = &rig.rt;
    let mut rx = rt.subscribe();
    let tmp = rig.tmp.path();

    // Assets: copied once, renamed on a clash, other formats refused.
    let logo = tmp.join("logo mark.png");
    std::fs::write(&logo, PIXEL).unwrap();
    let a = rt.asset_add(&logo.to_string_lossy()).unwrap();
    assert_eq!(
        (a.path.as_str(), a.width, a.height),
        ("assets/logo-mark.png", 1, 1)
    );
    assert_eq!(rt.asset_add(&logo.to_string_lossy()).unwrap(), a);
    let mut other = PIXEL.to_vec();
    other.push(0);
    std::fs::write(&logo, &other).unwrap();
    let b = rt.asset_add(&logo.to_string_lossy()).unwrap();
    assert_ne!(b.path, a.path);
    assert_eq!(rt.assets_list().unwrap().len(), 2);
    let exe = tmp.join("tool.exe");
    std::fs::write(&exe, b"MZ").unwrap();
    assert_eq!(
        rt.asset_add(&exe.to_string_lossy()).unwrap_err().code,
        IpcErrorCode::Invalid
    );

    // A renderer preview becomes the slide's thumbnail until the slide changes.
    let slide = rt.slide_create(None, "Title and Content").await.unwrap();
    let id = slide.slide.id.to_string();
    let b64 = base64::engine::general_purpose::STANDARD.encode(PIXEL);
    rt.slide_preview_png(&id, &format!("data:image/png;base64,{b64}"))
        .unwrap();
    let view = rt.deck_get().unwrap().slides.remove(0);
    assert_eq!(view.thumb.kind, ThumbKind::Preview);
    assert!(!view.thumb.stale);
    assert!(tmp
        .join("repo/decks/test")
        .join(view.thumb.path.unwrap())
        .is_file());
    assert!(rt.slide_preview_png(&id, "bm90IGEgcG5n").is_err());
    assert!(rt.slide_preview_png("s-missing", &b64).is_err());

    // Chart data through the declarative transforms.
    let source = json!({"kind": "inline", "table": {"columns": ["region", "units"], "rows": [["north", 30], ["south", 10], ["east", 20]], "loc": ""}});
    let table = rt
        .chart_compute(
            source.clone(),
            json!([{"op": "filter", "column": "units", "cmp": ">=", "value": 20}, {"op": "sort", "by": ["units"], "descending": true}]),
        )
        .await
        .unwrap();
    assert_eq!(
        table["rows"],
        json!([["north", 30], ["east", 20]]),
        "{table}"
    );
    let bad = rt
        .chart_compute(source, json!([{"op": "no_such_step"}]))
        .await
        .unwrap_err();
    assert!(!bad.message.is_empty());

    // Replacing the template keeps the slides and asks the session for a checkpoint first.
    let corp = repo()
        .join("evals/templates/corp/corp.potx")
        .to_string_lossy()
        .into_owned();
    let task = rt.template_replace(corp);
    let done = wait_for(&mut rx, |e| match e {
        DeckEvent::TaskDone {
            task_id,
            ok,
            message,
        } if *task_id == task => Some((*ok, message.clone())),
        _ => None,
    })
    .await;
    assert!(done.0, "{done:?}");
    let deck = rt.deck_get().unwrap();
    assert_eq!(deck.deck.template, "template/corp.potx");
    assert_eq!(deck.slides.len(), 1);
    // The preview was made for the old template.
    assert!(deck.slides[0].thumb.path.is_none());
    assert!(rig
        .host
        .labels
        .lock()
        .unwrap()
        .iter()
        .any(|l| l == "before replacing the template"));
    let bad_task = rt.template_replace(exe.to_string_lossy().into_owned());
    let refused = wait_for(&mut rx, |e| match e {
        DeckEvent::TaskDone { task_id, ok, .. } if *task_id == bad_task => Some(*ok),
        _ => None,
    })
    .await;
    assert!(!refused);
    rt.ws.close().await;
}
