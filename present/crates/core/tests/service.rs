//! D3 acceptance: the service layer the desktop shell calls, against the real kernel
//! and a scripted model.

use present_core::service::{ProjectRuntime, Workbench};
use present_core::session::ResolvedModel;
use present_protocol::deck::DeckMode;
use present_protocol::ui::{IpcErrorCode, ProjectEvent, ProposalDecision};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::Receiver;
use wb_providers::model::{ModelEvent, StopReason};
use wb_providers::ScriptedModelClient;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

struct Rig {
    tmp: tempfile::TempDir,
    wb: Workbench,
    rt: Arc<ProjectRuntime>,
}

fn scripted(scripts: Vec<Vec<ModelEvent>>) -> present_core::service::ModelFactory {
    let client = Arc::new(ScriptedModelClient::new(scripts));
    Arc::new(move |_spec: Option<&str>| {
        let client = client.clone();
        Box::pin(async move {
            Ok(ResolvedModel {
                client,
                caps: serde_json::from_value(
                    json!({"context_window": 200000, "tools": "native", "vision": false, "probed_at": "2026-11-15T00:00:00Z"}),
                )?,
                name: "scripted/model".into(),
                provider: serde_json::from_value(json!({"name": "scripted", "kind": "anthropic"}))?,
                max_output_tokens: None,
            })
        })
    })
}

async fn rig(scripts: Vec<Vec<ModelEvent>>) -> Option<Rig> {
    let tmp = tempfile::tempdir().unwrap();
    let wb = Workbench::new(&tmp.path().join("home"));
    {
        let mut o = wb.options.lock().unwrap();
        o.engine = "none".into();
        o.kernel_env = vec![(
            "PRESENT_FONT_DIRS".into(),
            repo().join("vendor/fonts").to_string_lossy().into_owned(),
        )];
    }
    wb.set_model_factory(scripted(scripts));
    let template = repo().join("evals/templates/plain/plain.pptx");
    let info = match wb
        .project_create(
            &tmp.path().join("p").to_string_lossy(),
            "Service test",
            &template.to_string_lossy(),
            Some(DeckMode::Structured),
        )
        .await
    {
        Ok(i) => i,
        Err(e) if e.message.contains("no kernel interpreter") => {
            eprintln!("skipped: {}", e.message);
            return None;
        }
        Err(e) => panic!("{e:?}"),
    };
    let rt = wb.project(&info.summary.id).unwrap();
    Some(Rig { tmp, wb, rt })
}

async fn wait_for<T>(
    rx: &mut Receiver<ProjectEvent>,
    mut pick: impl FnMut(&ProjectEvent) -> Option<T>,
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

fn slide_doc(title: &str) -> Value {
    json!({
        "layout": "Title and Content",
        "placeholders": {
            "title": {"type": "text", "text": {"paragraphs": [{"runs": [{"t": title}]}]}},
            "body": {"type": "text", "text": {"paragraphs": [{"runs": [{"t": "One point"}]}]}}
        }
    })
}

fn call(id: &str, name: &str, args: Value) -> ModelEvent {
    ModelEvent::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

fn stop_tools() -> ModelEvent {
    ModelEvent::Stop {
        reason: StopReason::ToolCalls,
        message: None,
    }
}

fn say(t: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Text { delta: t.into() },
        ModelEvent::Stop {
            reason: StopReason::End,
            message: None,
        },
    ]
}

#[tokio::test]
async fn manual_editing_history_and_registry() {
    let Some(rig) = rig(vec![]).await else { return };
    let rt = &rig.rt;
    let mut rx = rt.subscribe();

    let created = rt.slide_create(None, "Title and Content").await.unwrap();
    let id = created.slide.id.to_string();
    assert_eq!(rt.deck_get().unwrap().slides.len(), 1);
    wait_for(&mut rx, |e| {
        matches!(e, ProjectEvent::DeckChanged { by, .. } if by == "user").then_some(())
    })
    .await;

    // A stale hash is a conflict; the current one applies.
    let mut doc = serde_json::to_value(&created.slide).unwrap();
    doc["notes"] = json!({"paragraphs": [{"runs": [{"t": "Speaker note"}]}]});
    let edited: present_protocol::deck::Slide = serde_json::from_value(doc).unwrap();
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

    // The project is remembered and reopening returns the same runtime.
    let listed = rig.wb.projects_list();
    assert_eq!(listed.len(), 1);
    let again = rig
        .wb
        .project_open(&rig.tmp.path().join("p").to_string_lossy())
        .await
        .unwrap();
    assert_eq!(again.summary.id, rt.id);
    assert_eq!(
        again
            .deck
            .sections
            .iter()
            .map(|s| s.slides.len())
            .sum::<usize>(),
        1
    );

    // Layout and conversation state survive a close and reopen.
    rt.layout_set(json!({"panels": ["chat"]}));
    let conv = rt.conversation_new();
    rig.wb.project_close(&rt.id).await.unwrap();
    assert!(rig.wb.project(&rt.id).is_err());
    let info = rig
        .wb
        .project_open(&rig.tmp.path().join("p").to_string_lossy())
        .await
        .unwrap();
    let rt2 = rig.wb.project(&info.summary.id).unwrap();
    assert_eq!(rt2.layout_get(), Some(json!({"panels": ["chat"]})));
    assert_eq!(info.active_conversation, conv.id);
    rig.wb.project_close(&rt2.id).await.unwrap();
}

#[tokio::test]
async fn a_turn_streams_events_and_waits_for_the_users_decision() {
    let scripts = vec![
        vec![
            ModelEvent::Text {
                delta: "Adding a slide.".into(),
            },
            call(
                "t1",
                "slide_propose_create",
                json!({"slide": slide_doc("Revenue grew")}),
            ),
            stop_tools(),
        ],
        vec![
            call(
                "t2",
                "slide_propose_create",
                json!({"slide": slide_doc("Costs fell")}),
            ),
            stop_tools(),
        ],
        say("One slide added; the second was declined."),
        say("Nothing more to do."),
    ];
    let Some(rig) = rig(scripts).await else {
        return;
    };
    let rt = &rig.rt;
    let mut rx = rt.subscribe();

    let turn = rt.chat_send("main", "Add two slides", None).await.unwrap();
    assert_eq!(turn, "main-1");
    // A second message while the turn runs is refused.
    let busy = rt.chat_send("main", "And another", None).await.unwrap_err();
    assert_eq!(busy.code, IpcErrorCode::Invalid);

    let first = wait_for(&mut rx, |e| match e {
        ProjectEvent::Proposal { proposal } => Some(proposal.clone()),
        _ => None,
    })
    .await;
    assert_eq!(first.turn, turn);
    assert_eq!(first.slides.len(), 1);
    assert!(first.slides[0].before.is_none() && first.slides[0].after.is_some());
    // Nothing is written until the user decides.
    assert_eq!(rt.deck_get().unwrap().slides.len(), 0);
    rt.proposal_respond(&first.id, ProposalDecision::Accept)
        .unwrap();
    assert!(rt
        .proposal_respond(&first.id, ProposalDecision::Accept)
        .is_err());

    let second = wait_for(&mut rx, |e| match e {
        ProjectEvent::Proposal { proposal } => Some(proposal.clone()),
        _ => None,
    })
    .await;
    assert_eq!(rt.deck_get().unwrap().slides.len(), 1);
    rt.proposal_respond(
        &second.id,
        ProposalDecision::Reject {
            note: Some("Not this one".into()),
        },
    )
    .unwrap();

    let summary = wait_for(&mut rx, |e| match e {
        ProjectEvent::TurnDone { summary, .. } => Some(summary.clone()),
        ProjectEvent::TurnError { message, .. } => panic!("turn failed: {message}"),
        _ => None,
    })
    .await;
    assert_eq!(summary.slides_changed.len(), 1);
    assert!(summary.text.contains("declined"));
    assert_eq!(rt.deck_get().unwrap().slides.len(), 1);

    // The turn is one checkpoint; reverting it empties the deck again.
    let history = rt.history_list(None).unwrap();
    assert!(
        history.iter().any(|s| s.turn.as_deref() == Some("main-1")),
        "{history:?}"
    );
    rt.history_revert_turn("main-1").await.unwrap();
    assert_eq!(rt.deck_get().unwrap().slides.len(), 0);

    // The transcript is readable and the conversation is free again.
    let events = rt.conversation_transcript("main").unwrap();
    assert!(events.iter().any(|e| e["type"] == "proposal"));
    assert!(events.iter().any(|e| e["type"] == "decision"));
    let next = rt.chat_send("main", "Thanks", None).await.unwrap();
    assert_eq!(next, "main-2");
    wait_for(&mut rx, |e| {
        matches!(e, ProjectEvent::TurnDone { turn, .. } if turn == "main-2").then_some(())
    })
    .await;
}

#[tokio::test]
async fn cancelling_a_turn_drops_its_pending_proposal() {
    let scripts = vec![
        vec![
            call(
                "t1",
                "slide_propose_create",
                json!({"slide": slide_doc("Never applied")}),
            ),
            stop_tools(),
        ],
        say("Hello again."),
    ];
    let Some(rig) = rig(scripts).await else {
        return;
    };
    let rt = &rig.rt;
    let mut rx = rt.subscribe();
    rt.chat_send("main", "Add a slide", None).await.unwrap();
    let proposal = wait_for(&mut rx, |e| match e {
        ProjectEvent::Proposal { proposal } => Some(proposal.clone()),
        _ => None,
    })
    .await;
    rt.chat_cancel("main");
    assert!(rt
        .proposal_respond(&proposal.id, ProposalDecision::Accept)
        .is_err());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rt.deck_get().unwrap().slides.len(), 0);
    // The conversation accepts a new message.
    rt.chat_send("main", "Hello", None).await.unwrap();
    wait_for(&mut rx, |e| {
        matches!(e, ProjectEvent::TurnDone { .. }).then_some(())
    })
    .await;
}

#[tokio::test]
async fn references_exports_and_code_files() {
    let Some(rig) = rig(vec![]).await else { return };
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
        ProjectEvent::TaskDone {
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
        ProjectEvent::ExportDone { export } => Some(export.clone()),
        ProjectEvent::TaskDone {
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
async fn assets_previews_analyst_imports_and_template_replacement() {
    use base64::Engine;
    use present_protocol::ui::ThumbKind;
    let Some(rig) = rig(vec![]).await else { return };
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
    assert!(tmp.join("p").join(view.thumb.path.unwrap()).is_file());
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

    // Analyst import: only the chosen item is copied, with its provenance.
    let repo = tmp.join("analyst-repo");
    let figs = repo.join("out/figures/fit/20261104-113207");
    std::fs::create_dir_all(&figs).unwrap();
    std::fs::write(figs.join("residuals.png"), PIXEL).unwrap();
    std::fs::write(
        figs.join("residuals.json"),
        r#"{"run_id": "20261104-113207", "script": "scripts/fit.py", "args": ["--region", "north"], "title": "Residuals by week"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("data")).unwrap();
    std::fs::write(repo.join("data/secret.csv"), "a\n1\n").unwrap();
    let repo_abs = repo.to_string_lossy().into_owned();
    let items = rt.analyst_scan(&repo_abs).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        (items[0].kind.as_str(), items[0].title.as_str()),
        ("figure", "Residuals by week")
    );
    assert_eq!(items[0].script, "scripts/fit.py");
    let before = rt.refs_list().unwrap().len();
    let imported = rt.analyst_import(&repo_abs, &items[0].id).await.unwrap();
    assert_eq!(imported.title, "Residuals by week");
    assert_eq!(rt.refs_list().unwrap().len(), before + 1);
    // Importing the same item again adds nothing.
    rt.analyst_import(&repo_abs, &items[0].id).await.unwrap();
    assert_eq!(rt.refs_list().unwrap().len(), before + 1);
    assert!(rt
        .analyst_import(&repo_abs, "data/secret.csv")
        .await
        .is_err());

    // Replacing the template keeps the slides and takes a snapshot first.
    let corp = repo_root_template("corp/corp.potx");
    let task = rt.template_replace(corp);
    let done = wait_for(&mut rx, |e| match e {
        ProjectEvent::TaskDone {
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
    assert!(rt
        .history_list(None)
        .unwrap()
        .iter()
        .any(|s| s.label.as_deref() == Some("before replacing the template")));
    let bad_task = rt.template_replace(exe.to_string_lossy().into_owned());
    let refused = wait_for(&mut rx, |e| match e {
        ProjectEvent::TaskDone { task_id, ok, .. } if *task_id == bad_task => Some(*ok),
        _ => None,
    })
    .await;
    assert!(!refused);

    // A persisted policy change survives; an invalid one is refused and writes nothing.
    use present_protocol::ui::PolicyPatch;
    let patch = PolicyPatch {
        edits: Some("auto".into()),
        max_steps: Some(12),
        ..Default::default()
    };
    rt.persist_policy(&patch).unwrap();
    let view = rt.set_policy(PolicyPatch::default());
    assert_eq!((view.edits.as_str(), view.max_steps), ("auto", 12));
    let invalid = PolicyPatch {
        code: Some("whenever".into()),
        ..Default::default()
    };
    assert_eq!(
        rt.persist_policy(&invalid).unwrap_err().code,
        IpcErrorCode::Invalid
    );
    let saved = std::fs::read_to_string(tmp.join("p/.present/project.toml")).unwrap();
    assert!(
        saved.contains("max_steps = 12") && !saved.contains("whenever"),
        "{saved}"
    );

    // Keys: unknown providers are reported, nothing is stored.
    assert_eq!(
        rig.wb.provider_set_key("nope", "sk-x").unwrap_err().code,
        IpcErrorCode::NotFound
    );
}

fn repo_root_template(rel: &str) -> String {
    repo()
        .join("evals/templates")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

/// Keys are written to the home `.env` under the provider's variable; nothing else stores
/// them, and a leftover `keychain:` reference is refused with the way out.
#[test]
fn provider_keys_are_stored_in_the_home_env_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        r#"
[network]
proxy = "http://proxy.corp:8080"

[[providers]]
name = "gateway"
kind = "anthropic"
base_url = "https://llm-gateway.corp.example"
key = "env:PRESENT_TEST_SERVICE_GATEWAY_KEY"

[[providers]]
name = "old"
kind = "anthropic"
key = "keychain:old"

[[providers]]
name = "bare"
kind = "openai-compatible"
"#,
    )
    .unwrap();
    let wb = Workbench::new(&home);
    // Starting the service points the egress ledger into a home directory.
    assert_eq!(
        wb_net::gate()
            .ledger_path()
            .and_then(|p| p.file_name().map(|n| n.to_owned())),
        Some("egress.jsonl".into())
    );

    wb.provider_set_key("gateway", "  sk-secret  ").unwrap();
    let env_file = home.join(".env");
    let text = std::fs::read_to_string(&env_file).unwrap();
    assert_eq!(text, "PRESENT_TEST_SERVICE_GATEWAY_KEY=\"sk-secret\"\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&env_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The stored key is what the provider layer resolves.
    let keys = wb_providers::keys::KeyStores::for_home(&home);
    let reference = wb_providers::keys::KeyRef::parse("env:PRESENT_TEST_SERVICE_GATEWAY_KEY");
    assert_eq!(keys.get(&reference.unwrap()).as_deref(), Some("sk-secret"));

    let empty = wb.provider_set_key("gateway", "   ").unwrap_err();
    assert_eq!(empty.code, IpcErrorCode::Invalid);
    let old = wb.provider_set_key("old", "sk-x").unwrap_err();
    assert_eq!(old.code, IpcErrorCode::Invalid);
    assert!(
        old.message.contains("env:<VAR>") && old.message.contains(".env"),
        "{}",
        old.message
    );
    let bare = wb.provider_set_key("bare", "sk-x").unwrap_err();
    assert_eq!(bare.code, IpcErrorCode::Invalid);
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), text);
}
