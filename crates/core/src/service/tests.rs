//! Service-level unit tests with the scripted model client (no network; the tests that need a
//! kernel use the stdlib-only fake in `tests/fake_kernel.py`).

use super::*;
use crate::providers::ScriptedModelClient;
use analyst_protocol::model::{ModelEvent, StopReason};
use analyst_protocol::transcript::Decision;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::broadcast;

struct Fixture {
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    wb: Arc<Workbench>,
    session: String,
    rt: Arc<SessionRuntime>,
}

async fn fixture() -> Fixture {
    fixture_with(|_| {}).await
}

/// The fake kernel as the local kernel launcher (it runs scripts with `python3`).
fn fake_kernel(opts: &mut WorkbenchOptions) {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_kernel.py");
    opts.kernel_launcher = Some(vec!["python3".into(), script.to_string_lossy().to_string()]);
    opts.session_python = Some("python3".into());
}

async fn fixture_with(configure: impl FnOnce(&mut WorkbenchOptions)) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let mut opts = WorkbenchOptions::new(home.path().to_path_buf());
    opts.autostart_kernel = false;
    opts.init = crate::init::InitOptions {
        run_uv: false,
        git: true,
    };
    configure(&mut opts);
    let wb = Arc::new(Workbench::new(opts).unwrap());
    let info = wb
        .create_session(&repo.path().to_string_lossy(), None, None, true)
        .await
        .unwrap();
    let rt = wb.session(&info.summary.id).unwrap();
    Fixture {
        _home: home,
        _repo: repo,
        wb,
        session: info.summary.id,
        rt,
    }
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

async fn next_matching<F>(rx: &mut Subscription, pred: F) -> SessionEvent
where
    F: Fn(&SessionEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(e)) if pred(&e) => return e,
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            other => panic!("event not received: {other:?}"),
        }
    }
}

#[tokio::test]
async fn session_lifecycle_and_fs_ops() {
    let fx = fixture().await;
    let info = fx.wb.session_info(&fx.session).await.unwrap();
    assert!(info.summary.open);
    assert_eq!(
        info.target_kind,
        analyst_protocol::config::TargetKind::Local
    );
    assert_eq!(info.kernel, KernelState::Stopped);
    assert_eq!(info.conversations.len(), 1);
    assert_eq!(info.active_conversation, info.conversations[0].id);
    assert_eq!(
        info.policy.edits,
        analyst_protocol::config::EditPolicy::Review
    );

    let list = fx.wb.sessions_list().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].open);
    let renamed = fx.wb.rename_session(&fx.session, "decay").unwrap();
    assert_eq!(renamed.name, "decay");

    // Tree: template dirs, .venv and __pycache__ hidden, out/ collapsed.
    std::fs::create_dir_all(fx.rt.repo.join(".venv/bin")).unwrap();
    std::fs::create_dir_all(fx.rt.repo.join("analysis/__pycache__")).unwrap();
    std::fs::write(fx.rt.repo.join("analysis/__pycache__/x.pyc"), "x").unwrap();
    let tree = fx.rt.fs_tree(None, Some(3)).unwrap();
    let names: Vec<&str> = tree.iter().map(|e| e.path.as_str()).collect();
    assert!(names.contains(&"scripts"));
    assert!(names.contains(&"analysis"));
    assert!(names.contains(&".analyst"));
    assert!(!names.contains(&".venv"));
    assert!(!names.contains(&".git"));
    let out = tree.iter().find(|e| e.path == "out").unwrap();
    assert!(out.children.is_none());
    let analysis = tree.iter().find(|e| e.path == "analysis").unwrap();
    assert!(analysis
        .children
        .as_ref()
        .unwrap()
        .iter()
        .all(|c| !c.path.contains("__pycache__")));

    // Write + conflict detection through the runtime.
    let c = fx.rt.fs_read("analysis/__init__.py").unwrap();
    let w = fx
        .rt
        .fs_write("analysis/__init__.py", "x = 1\n", Some(&c.hash))
        .unwrap();
    let err = fx
        .rt
        .fs_write("analysis/__init__.py", "y = 1\n", Some(&c.hash))
        .unwrap_err();
    assert_eq!(err.code, IpcErrorCode::Conflict);
    fx.rt
        .fs_write("analysis/__init__.py", "y = 1\n", Some(&w.hash))
        .unwrap();

    // Layout round trip and policy override.
    assert_eq!(fx.wb.layout_get(&fx.session).unwrap(), None);
    fx.wb.layout_save(&fx.session, "{\"a\":1}").unwrap();
    assert_eq!(
        fx.wb.layout_get(&fx.session).unwrap().as_deref(),
        Some("{\"a\":1}")
    );
    let p = fx
        .rt
        .set_policy(&PolicyPatch {
            edits: Some(analyst_protocol::config::EditPolicy::Auto),
            max_steps: Some(7),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(p.edits, analyst_protocol::config::EditPolicy::Auto);
    assert_eq!(p.max_steps, 7);
    assert_eq!(fx.rt.effective_policy().max_steps, 7);
    assert_eq!(
        crate::config::load_project(&fx.rt.repo)
            .unwrap()
            .policy
            .max_steps,
        40,
        "override is not written to project.toml"
    );

    // Conversations.
    let c2 = fx.rt.conversation_new(Some("second")).unwrap();
    assert_eq!(
        fx.rt.meta().active_conversation.as_deref(),
        Some(c2.id.as_str())
    );
    fx.rt
        .conversation_select(&info.active_conversation)
        .unwrap();
    assert_eq!(
        fx.rt.conversation_select("nope").unwrap_err().code,
        IpcErrorCode::NotFound
    );
    assert!(fx.rt.conversation_transcript(&c2.id).unwrap().is_empty());

    // Kernel-dependent commands: `kernel_offline` when no kernel can start (no tooling
    // interpreter), `not_found` from the kernel when one is available in this checkout.
    let err = fx.rt.run_cancel("x").await.unwrap_err();
    assert!(
        matches!(
            err.code,
            IpcErrorCode::KernelOffline | IpcErrorCode::NotFound
        ),
        "{err:?}"
    );

    fx.wb.close_session(&fx.session).await.unwrap();
    assert_eq!(
        fx.wb.session_info(&fx.session).await.unwrap_err().code,
        IpcErrorCode::NotFound
    );
    assert!(!fx.wb.sessions_list().unwrap()[0].open);
    // Reopen is idempotent.
    let again = fx.wb.open_session(&fx.session).await.unwrap();
    assert_eq!(again.summary.name, "decay");
    fx.wb.close_session(&fx.session).await.unwrap();
}

#[tokio::test]
async fn ui_approver_round_trip_accepts_with_modified_content() {
    let fx = fixture().await;
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            ModelEvent::Text {
                delta: "Editing.".into(),
            },
            call(
                "s",
                "declare_scope",
                json!({"files": ["analysis/__init__.py"]}),
            ),
            call(
                "e",
                "fs_propose_edit",
                json!({"path": "analysis/__init__.py", "content": "PROPOSED = 1\n"}),
            ),
            stop_tools(),
        ],
        vec![
            ModelEvent::Text {
                delta: "Done.".into(),
            },
            ModelEvent::Usage {
                input_tokens: 11,
                output_tokens: 3,
            },
        ],
    ]));
    fx.rt.set_model_override(Some(model.clone()));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    let turn = agent_runner::start_turn(fx.rt.clone(), conv.clone(), "edit the init", vec![])
        .await
        .unwrap();
    assert!(conv.busy());
    assert_eq!(
        agent_runner::start_turn(fx.rt.clone(), conv.clone(), "again", vec![])
            .await
            .unwrap_err()
            .code,
        IpcErrorCode::Invalid,
        "busy conversation rejects a second turn"
    );
    let started = next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnStarted { .. })).await;
    assert!(matches!(started, SessionEvent::TurnStarted { turn: t, .. } if t == turn));
    let text = next_matching(&mut rx, |e| matches!(e, SessionEvent::AgentText { .. })).await;
    assert!(matches!(text, SessionEvent::AgentText { delta, .. } if delta == "Editing."));
    let proposal = next_matching(&mut rx, |e| matches!(e, SessionEvent::Proposal { .. })).await;
    let SessionEvent::Proposal { proposal } = proposal else {
        unreachable!()
    };
    assert_eq!(
        proposal.kind,
        analyst_protocol::transcript::ProposalKind::Edit
    );
    assert_eq!(proposal.path.as_deref(), Some("analysis/__init__.py"));
    assert_eq!(proposal.proposed.as_deref(), Some("PROPOSED = 1\n"));
    assert!(proposal
        .original
        .as_deref()
        .unwrap()
        .contains("Shared analysis library"));
    assert!(proposal.diff.as_ref().unwrap().contains("+PROPOSED = 1"));
    assert!(proposal.in_scope);
    assert_eq!(proposal.reason, ProposalReason::Review);
    assert_eq!(proposal.conversation, conv_id);

    // Accept with the user's own content.
    fx.rt
        .proposal_respond(
            &proposal.id,
            ProposalDecision::Accept {
                content: Some("USER = 2\n".into()),
            },
        )
        .unwrap();
    assert_eq!(
        fx.rt
            .proposal_respond(&proposal.id, ProposalDecision::Reject { reason: None })
            .unwrap_err()
            .code,
        IpcErrorCode::NotFound,
        "already resolved"
    );
    let resolved = next_matching(&mut rx, |e| {
        matches!(e, SessionEvent::ProposalResolved { .. })
    })
    .await;
    assert!(matches!(
        resolved,
        SessionEvent::ProposalResolved { proposal_id, decision: Decision::Accept, by: analyst_protocol::transcript::DecidedBy::User }
        if proposal_id == proposal.id
    ));
    let result = next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::AgentToolResult { id, .. } if id == "e"),
    )
    .await;
    assert!(
        matches!(result, SessionEvent::AgentToolResult { ok: true, summary, .. } if summary.contains("applied analysis/__init__.py"))
    );
    let done = next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;
    let SessionEvent::TurnDone { summary, .. } = done else {
        unreachable!()
    };
    assert_eq!(summary.text, "Done.");
    assert_eq!(summary.files_changed, vec!["analysis/__init__.py"]);
    assert_eq!(
        std::fs::read_to_string(fx.rt.repo.join("analysis/__init__.py")).unwrap(),
        "USER = 2\n"
    );
    // The model was told the user modified the proposal.
    let reqs = model.recorded();
    let tool_result = reqs[1].messages[2]
        .content
        .iter()
        .find_map(|p| match p {
            analyst_protocol::model::ContentPart::ToolResult {
                tool_call_id,
                content,
                ..
            } if tool_call_id == "e" => Some(content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(tool_result.contains("user_modified"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!conv.busy());
    let info = fx.rt.info().await;
    assert_eq!(info.conversations[0].turns, 1);
    fx.wb.close_session(&fx.session).await.unwrap();
}

#[tokio::test]
async fn chat_cancel_rejects_pending_proposal() {
    let fx = fixture().await;
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call("s", "declare_scope", json!({"files": ["scripts/new.py"]})),
            call(
                "c",
                "fs_propose_create",
                json!({"path": "scripts/new.py", "content": "x = 1\n"}),
            ),
            stop_tools(),
        ],
        vec![ModelEvent::Text {
            delta: "never".into(),
        }],
    ]));
    fx.rt.set_model_override(Some(model.clone()));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    agent_runner::start_turn(fx.rt.clone(), conv.clone(), "create", vec![])
        .await
        .unwrap();
    let SessionEvent::Proposal { proposal } =
        next_matching(&mut rx, |e| matches!(e, SessionEvent::Proposal { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(
        proposal.kind,
        analyst_protocol::transcript::ProposalKind::Create
    );
    assert!(proposal.original.is_none());
    agent_runner::cancel_turn(&fx.rt, &conv).await;
    let resolved = next_matching(&mut rx, |e| {
        matches!(e, SessionEvent::ProposalResolved { .. })
    })
    .await;
    assert!(matches!(
        resolved,
        SessionEvent::ProposalResolved { proposal_id, decision: Decision::Reject, .. } if proposal_id == proposal.id
    ));
    let err = next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnError { .. })).await;
    assert!(matches!(err, SessionEvent::TurnError { message, .. } if message == "cancelled"));
    assert!(!conv.busy());
    assert!(!fx.rt.repo.join("scripts/new.py").exists());
    assert_eq!(
        fx.rt
            .proposal_respond(&proposal.id, ProposalDecision::Accept { content: None })
            .unwrap_err()
            .code,
        IpcErrorCode::NotFound
    );
    assert_eq!(model.calls(), 1, "the second model call never happened");
    // The conversation is usable again.
    let model2 = Arc::new(ScriptedModelClient::new(vec![vec![ModelEvent::Text {
        delta: "ok".into(),
    }]]));
    fx.rt.set_model_override(Some(model2.clone()));
    agent_runner::start_turn(fx.rt.clone(), conv.clone(), "hello", vec![])
        .await
        .unwrap();
    let done = next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;
    assert!(matches!(done, SessionEvent::TurnDone { summary, .. } if summary.text == "ok"));
    fx.wb.close_session(&fx.session).await.unwrap();
}

#[tokio::test]
async fn package_proposal_and_terminal_through_workbench() {
    let fx = fixture().await;
    // A package request shows up as a package proposal with the package list.
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call("p", "pkg_add", json!({"packages": ["numpy"]})),
            stop_tools(),
        ],
        vec![ModelEvent::Text {
            delta: "fine".into(),
        }],
    ]));
    fx.rt.set_model_override(Some(model));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    agent_runner::start_turn(fx.rt.clone(), conv.clone(), "add numpy", vec![])
        .await
        .unwrap();
    let SessionEvent::Proposal { proposal } =
        next_matching(&mut rx, |e| matches!(e, SessionEvent::Proposal { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(
        proposal.kind,
        analyst_protocol::transcript::ProposalKind::Package
    );
    assert_eq!(
        proposal.packages.as_deref(),
        Some(&["numpy".to_string()][..])
    );
    assert_eq!(proposal.reason, ProposalReason::Ask);
    fx.rt
        .proposal_respond(
            &proposal.id,
            ProposalDecision::Reject {
                reason: Some("not now".into()),
            },
        )
        .unwrap();
    let result = next_matching(&mut rx, |e| {
        matches!(e, SessionEvent::AgentToolResult { .. })
    })
    .await;
    assert!(
        matches!(result, SessionEvent::AgentToolResult { ok: false, summary, .. } if summary.contains("not now"))
    );
    next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;

    // Terminal through the workbench: events arrive on the session channel.
    // The desktop shell awaits this from a command handler, which needs a `Send` future.
    fn assert_send<T: Send>(t: T) -> T {
        t
    }
    let opened = assert_send(fx.wb.terminal_open(&fx.session, 80, 24))
        .await
        .unwrap();
    // The isolation state is part of the result, and agrees with what this machine can do.
    let probe = wb_sandbox::no_network::probe();
    assert_eq!(opened.network_isolated, probe.is_isolated());
    assert_eq!(opened.network_isolation, probe.label());
    assert_eq!(opened.network_isolation_note.is_none(), probe.is_isolated());
    let tid = opened.terminal_id;
    fx.wb.terminal_write(&tid, "echo work-bench\n").unwrap();
    let data = next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::TerminalData { data, .. } if data.contains("work-bench")),
    )
    .await;
    assert!(matches!(data, SessionEvent::TerminalData { terminal_id, .. } if terminal_id == tid));
    fx.wb.terminal_resize(&tid, 120, 40).unwrap();
    fx.wb.terminal_close(&tid).unwrap();
    assert_eq!(
        fx.wb.terminal_write(&tid, "x").unwrap_err().code,
        IpcErrorCode::NotFound
    );
    next_matching(&mut rx, |e| matches!(e, SessionEvent::TerminalExit { .. })).await;
    fx.wb.close_session(&fx.session).await.unwrap();
}

#[tokio::test]
async fn watcher_reports_changes_and_terminal_runs() {
    let fx = fixture().await;
    let mut rx = fx.rt.subscribe();
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(fx.rt.repo.join("notes.md"), "# changed\n").unwrap();
    let changed = next_matching(&mut rx, |e| matches!(e, SessionEvent::FilesChanged { .. })).await;
    assert!(
        matches!(changed, SessionEvent::FilesChanged { paths } if paths.contains(&"notes.md".to_string()))
    );
    // A figure appearing under out/terminal/<stamp>/figures is a terminal run.
    let figs = fx
        .rt
        .repo
        .join("out/terminal/20260101-000000-abcdef/figures");
    std::fs::create_dir_all(&figs).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    std::fs::write(figs.join("fig-1.png"), b"\x89PNG").unwrap();
    let started = next_matching(&mut rx, |e| matches!(e, SessionEvent::RunStarted { .. })).await;
    let SessionEvent::RunStarted { run } = started else {
        unreachable!()
    };
    assert_eq!(run.meta.run_id, "terminal-20260101-000000-abcdef");
    assert_eq!(run.meta.script, "terminal");
    next_matching(&mut rx, |e| matches!(e, SessionEvent::RunDone { .. })).await;
    let art = next_matching(&mut rx, |e| matches!(e, SessionEvent::RunArtifact { .. })).await;
    assert!(
        matches!(art, SessionEvent::RunArtifact { figure: Some(f), .. } if f.path.ends_with("figures/fig-1.png"))
    );
    assert!(fx
        .rt
        .repo
        .join("out/terminal/20260101-000000-abcdef/run.json")
        .is_file());
    let groups = fx.rt.figures_list(None).await;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].run.meta.run_id, "terminal-20260101-000000-abcdef");
    assert_eq!(groups[0].figures.len(), 1);
    let data = fx
        .rt
        .figure_data("out/terminal/20260101-000000-abcdef/figures/fig-1.png")
        .await
        .unwrap();
    assert_eq!(data.mime, "image/png");
    assert_eq!(data.base64, "iVBORw==");
    let runs = fx.rt.runs_list(None);
    assert_eq!(runs[0].meta.run_id, "terminal-20260101-000000-abcdef");
    fx.wb.close_session(&fx.session).await.unwrap();
}

#[test]
fn models_and_targets_from_config() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
[defaults]
model = "ollama/llama"
[[providers]]
name = "ollama"
kind = "openai-compatible"
base_url = "http://localhost:11434/v1"
[[providers]]
name = "anthropic"
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
[[models]]
provider = "anthropic"
id = "claude-x"
context_window = 200000
tools = "native"
vision = true
[[targets]]
name = "lab"
kind = "ssh"
host = "lab-gpu"
data_dirs = ["/data"]
"#,
    )
    .unwrap();
    let wb = Workbench::new(WorkbenchOptions::new(home.path().to_path_buf())).unwrap();
    let models = wb.models_list();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].name, "ollama/llama");
    assert!(models[0].local);
    assert!(models[0].capabilities.is_none());
    assert_eq!(models[1].kind, ProviderKind::Anthropic);
    assert!(!models[1].local);
    assert_eq!(
        models[1].capabilities.as_ref().unwrap().context_window,
        200000
    );
    let targets = wb.targets_list();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].name, "local");
    assert_eq!(targets[1].kind, TargetKind::Ssh);
    assert_eq!(targets[1].host.as_deref(), Some("lab-gpu"));
    assert_eq!(targets[1].remote_root.as_deref(), Some("~/analyst"));
    assert_eq!(
        wb.target_bootstrap("local").unwrap_err().code,
        IpcErrorCode::Invalid
    );
    assert_eq!(
        wb.target_bootstrap("nope").unwrap_err().code,
        IpcErrorCode::NotFound
    );
    let info = wb.app_info();
    assert_eq!(info.home_abs, home.path().to_string_lossy());
}

// ---- regression tests for the November 2026 review ----

/// U4: parallel `session_open` calls build one runtime.
#[tokio::test]
async fn parallel_opens_build_one_runtime() {
    let fx = fixture().await;
    assert_eq!(fx.wb.runtimes_built(), 1);
    fx.wb.close_session(&fx.session).await.unwrap();
    let mut joins = Vec::new();
    for _ in 0..16 {
        let wb = fx.wb.clone();
        let id = fx.session.clone();
        joins.push(tokio::spawn(async move { wb.open_session(&id).await }));
    }
    for j in joins {
        assert!(j.await.unwrap().unwrap().summary.open);
    }
    assert_eq!(fx.wb.runtimes_built(), 2, "one runtime for sixteen opens");
    // A subscription on a session that is not open is `not_found` (B2).
    fx.wb.close_session(&fx.session).await.unwrap();
    assert_eq!(
        fx.wb.session(&fx.session).err().unwrap().code,
        IpcErrorCode::NotFound
    );
}

/// G4 and G7 through the runtime: symlinks out of the repo, `.git`, and `.analyst` are refused
/// by every file command, by `figure_data`, and by chat attachments.
#[cfg(unix)]
#[tokio::test]
async fn file_commands_and_attachments_stay_inside_the_repo() {
    let fx = fixture().await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "TOP-SECRET-TOKEN").unwrap();
    std::fs::write(outside.path().join("fig.png"), b"\x89PNG").unwrap();
    std::os::unix::fs::symlink(outside.path(), fx.rt.repo.join("ext")).unwrap();
    let abs = outside.path().join("secret.txt");
    let abs = abs.to_string_lossy();

    assert_eq!(
        fx.rt.fs_read("ext/secret.txt").unwrap_err().code,
        IpcErrorCode::Invalid
    );
    assert_eq!(fx.rt.fs_read(&abs).unwrap_err().code, IpcErrorCode::Invalid);
    assert_eq!(
        fx.rt.fs_read_binary("ext/fig.png").unwrap_err().code,
        IpcErrorCode::Invalid
    );
    assert_eq!(
        fx.rt.figure_data("ext/fig.png").await.unwrap_err().code,
        IpcErrorCode::Invalid
    );
    assert_eq!(
        fx.rt.figure_data("../x.png").await.unwrap_err().code,
        IpcErrorCode::Invalid
    );
    assert_eq!(
        fx.rt.fs_write("ext/new.txt", "x", None).unwrap_err().code,
        IpcErrorCode::Invalid
    );
    assert_eq!(
        fx.rt.fs_delete("ext/secret.txt").unwrap_err().code,
        IpcErrorCode::Invalid
    );
    assert!(!outside.path().join("new.txt").exists());
    assert_eq!(
        fx.rt
            .fs_write(".git/hooks/pre-commit", "#!/bin/sh\n", None)
            .unwrap_err()
            .code,
        IpcErrorCode::PolicyDenied
    );
    assert_eq!(
        fx.rt
            .fs_write(
                ".analyst/project.toml",
                "[policy]\nedits = \"auto\"\n",
                None
            )
            .unwrap_err()
            .code,
        IpcErrorCode::PolicyDenied
    );
    assert_eq!(
        fx.rt.fs_rename("notes.md", ".git/x").unwrap_err().code,
        IpcErrorCode::PolicyDenied
    );
    assert_eq!(
        fx.rt.fs_delete(".git").unwrap_err().code,
        IpcErrorCode::PolicyDenied
    );
    assert!(fx.rt.fs_read(".analyst/project.toml").is_ok(), "reads work");
    assert!(!fx.rt.repo.join(".git/hooks/pre-commit").exists());
    assert_eq!(
        fx.rt
            .out_read("../../etc/passwd".into(), None)
            .await
            .unwrap_err()
            .code,
        IpcErrorCode::Invalid
    );
    assert_eq!(
        fx.rt.run_get("../x").unwrap_err().code,
        IpcErrorCode::NotFound
    );

    // Attachments: nothing outside the repo reaches the model.
    let model = Arc::new(ScriptedModelClient::new(vec![vec![ModelEvent::Text {
        delta: "ok".into(),
    }]]));
    fx.rt.set_model_override(Some(model.clone()));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    agent_runner::start_turn(
        fx.rt.clone(),
        conv,
        "look",
        vec![
            Attachment::File {
                path: abs.to_string(),
            },
            Attachment::File {
                path: "ext/secret.txt".into(),
            },
            Attachment::File {
                path: "../../../etc/hostname".into(),
            },
            Attachment::Figure {
                path: "ext/fig.png".into(),
            },
            Attachment::File {
                path: "analysis/__init__.py".into(),
            },
        ],
    )
    .await
    .unwrap();
    next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;
    let sent = format!("{:?}", model.recorded()[0].messages);
    assert!(!sent.contains("TOP-SECRET-TOKEN"), "{sent}");
    assert!(sent.contains("could not be attached"), "{sent}");
    assert!(
        sent.contains("Attached file analysis/__init__.py"),
        "{sent}"
    );
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// U1: a subscriber that falls behind gets `resync`; a terminal flood does not cause one.
#[tokio::test]
async fn lagged_subscriber_gets_resync() {
    let fx = fixture().await;
    let mut rx = fx.rt.subscribe();
    let hub = fx.rt.hub();
    for i in 0..(events::CONTROL_CAPACITY * 3) {
        hub.terminal_data("t9", &format!("{i}\n"));
    }
    fx.rt.emit(SessionEvent::PackagesChanged);
    assert!(
        matches!(rx.recv().await.unwrap(), SessionEvent::PackagesChanged),
        "terminal output must not displace control events"
    );
    assert!(matches!(
        rx.recv().await.unwrap(),
        SessionEvent::TerminalData { .. }
    ));
    for _ in 0..(events::CONTROL_CAPACITY + 10) {
        fx.rt.emit(SessionEvent::IndexChanged);
    }
    assert!(matches!(rx.recv().await.unwrap(), SessionEvent::Resync));
    assert!(matches!(
        rx.recv().await.unwrap(),
        SessionEvent::IndexChanged
    ));
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// U3: `figures_list` stops at the limit and reads the run history once.
#[tokio::test]
async fn figures_list_is_bounded_for_a_long_history() {
    let fx = fixture().await;
    let n = 600usize;
    let session = fx.rt.session_snapshot();
    for i in 0..n {
        let id = format!("20260101-{:06}-aaaaaa", i);
        let dir = fx.rt.repo.join("out/fit").join(&id);
        std::fs::create_dir_all(dir.join("figures")).unwrap();
        std::fs::write(dir.join("figures/f.png"), b"\x89PNG").unwrap();
        let started = format!(
            "2026-01-01T{:02}:{:02}:{:02}Z",
            i / 3600,
            (i / 60) % 60,
            i % 60
        );
        std::fs::write(
            dir.join("run.json"),
            json!({"run_id": id, "script": "scripts/fit.py", "args": [], "started_at": started})
                .to_string(),
        )
        .unwrap();
        let meta = analyst_protocol::kernel::RunMeta {
            run_id: id.clone(),
            script: "scripts/fit.py".into(),
            args: vec![],
            status: analyst_protocol::kernel::RunStatus::Done,
            exit_code: Some(0),
            started_at: started,
            finished_at: None,
            out_dir: format!("out/fit/{id}"),
            sandbox: "none".into(),
            tree_hash: None,
            pid: None,
            host: "local".into(),
        };
        // Twice, as real runs are (start and finish).
        session.append_run(&meta).unwrap();
        session.append_run(&meta).unwrap();
    }
    let t0 = std::time::Instant::now();
    let groups = fx.rt.figures_list(Some(20)).await;
    let took = t0.elapsed();
    assert_eq!(groups.len(), 20);
    assert_eq!(
        groups[0].run.meta.run_id,
        format!("20260101-{:06}-aaaaaa", n - 1),
        "newest first"
    );
    assert_eq!(
        groups[0].run.meta.exit_code,
        Some(0),
        "from the run history"
    );
    assert_eq!(groups[0].figures.len(), 1);
    assert!(
        took < Duration::from_secs(2),
        "figures_list took {took:?} for {n} runs (was 5.7 s for 500)"
    );
    // The default limit also stays fast.
    let t0 = std::time::Instant::now();
    assert_eq!(fx.rt.figures_list(None).await.len(), 100);
    assert!(t0.elapsed() < Duration::from_secs(3));
    // The watcher does not hold a watch per historical run directory (U2).
    assert!(fx.rt.watched_dirs() < 200, "{}", fx.rt.watched_dirs());
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// U2: `.git`, `.venv`, `node_modules`, `__pycache__`, and old run directories are not watched.
#[tokio::test]
async fn watcher_skips_heavy_directories() {
    let fx = fixture().await;
    fx.wb.close_session(&fx.session).await.unwrap();
    let repo = fx.rt.repo.clone();
    for i in 0..40 {
        std::fs::create_dir_all(repo.join(format!(".venv/lib/pkg{i}/sub"))).unwrap();
        std::fs::create_dir_all(repo.join(format!("node_modules/m{i}"))).unwrap();
        std::fs::create_dir_all(repo.join(format!("analysis/__pycache__/d{i}"))).unwrap();
        std::fs::create_dir_all(repo.join(format!("out/fit/2026010{}-{i:06}/figures", i % 9)))
            .unwrap();
    }
    std::fs::create_dir_all(repo.join("analysis/sub/deeper")).unwrap();
    fx.wb.open_session(&fx.session).await.unwrap();
    let rt = fx.wb.session(&fx.session).unwrap();
    let watched = rt.watched_dirs();
    assert!(watched >= 5, "{watched}");
    assert!(watched < 30, "too many watches: {watched}");
    // Changes in a nested source directory are still seen ...
    let mut rx = rt.subscribe();
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(repo.join("analysis/sub/deeper/m.py"), "x = 1\n").unwrap();
    let e = next_matching(&mut rx, |e| matches!(e, SessionEvent::FilesChanged { .. })).await;
    assert!(
        matches!(e, SessionEvent::FilesChanged { paths } if paths.contains(&"analysis/sub/deeper/m.py".to_string()))
    );
    // ... and so are files in a directory created after the session opened.
    std::fs::create_dir_all(repo.join("newpkg/inner")).unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    std::fs::write(repo.join("newpkg/inner/a.py"), "y = 2\n").unwrap();
    let e = next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::FilesChanged { paths } if paths.iter().any(|p| p == "newpkg/inner/a.py")),
    )
    .await;
    drop(e);
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// B2/U8: commands that need no kernel never wait for one, and `index_get` fails fast while
/// the kernel is not ready.
#[tokio::test]
async fn kernel_free_commands_do_not_wait_and_index_get_fails_fast() {
    // A "kernel" that never answers: a start takes until the 90 s timeout.
    let fx = fixture_with(|o| {
        o.kernel_launcher = Some(vec!["sh".into(), "-c".into(), "sleep 60".into()]);
        o.autostart_kernel = true;
    })
    .await;
    let t0 = std::time::Instant::now();
    let info = fx.wb.session_info(&fx.session).await.unwrap();
    assert!(matches!(
        info.kernel,
        KernelState::Starting | KernelState::Stopped
    ));
    fx.rt.fs_tree(None, Some(2)).unwrap();
    assert!(fx.rt.runs_list(None).is_empty());
    assert!(fx.rt.figures_list(None).await.is_empty());
    fx.rt
        .conversation_transcript(&info.active_conversation)
        .unwrap();
    fx.rt.layout_get().unwrap();
    assert_eq!(
        fx.rt.run_get("nope").unwrap_err().code,
        IpcErrorCode::NotFound
    );
    let err = fx.rt.index_get().await.unwrap_err();
    assert_eq!(err.code, IpcErrorCode::KernelOffline);
    let err = fx.rt.check_run(None).await.unwrap_err();
    assert_eq!(err.code, IpcErrorCode::KernelOffline);
    assert!(fx.rt.env_packages().await.is_err());
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "commands waited for the kernel: {:?}",
        t0.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fx.rt.kernel_state(), KernelState::Starting);
    let t0 = std::time::Instant::now();
    fx.wb.close_session(&fx.session).await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(20));
}

/// R7: after the kernel client is replaced, the old client's forwarder is gone and does not
/// report "connection lost" for it. Also: `ready` after every start, `session_changed` with
/// the sandbox, and `index_changed` when the index is rebuilt.
#[tokio::test]
async fn forwarder_stops_when_the_kernel_is_replaced() {
    let fx = fixture_with(fake_kernel).await;
    let mut rx = fx.rt.subscribe();
    fx.rt.ensure_kernel().await.unwrap();
    next_matching(&mut rx, |e| {
        matches!(
            e,
            SessionEvent::KernelState {
                state: KernelState::Ready,
                ..
            }
        )
    })
    .await;
    let changed = next_matching(&mut rx, |e| {
        matches!(e, SessionEvent::SessionChanged { .. })
    })
    .await;
    let SessionEvent::SessionChanged { info } = changed else {
        unreachable!()
    };
    assert_eq!(info.kernel, KernelState::Ready);
    assert_eq!(info.sandbox.as_deref(), Some("none"));
    next_matching(&mut rx, |e| matches!(e, SessionEvent::IndexChanged)).await;
    assert!(fx.rt.index_get().await.is_ok());

    // Replace the client, as a target switch does.
    fx.rt.shutdown_kernel().await;
    assert_eq!(fx.rt.kernel_state(), KernelState::Stopped);
    fx.rt.ensure_kernel().await.unwrap();
    assert_eq!(fx.rt.kernel_state(), KernelState::Ready);
    // The old forwarder ticked every 2 s and would now report the old client as lost.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut ready_seen = 0;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(SessionEvent::KernelState { state, message, .. })) => {
                assert!(
                    !matches!(state, KernelState::Error | KernelState::Offline),
                    "stale forwarder reported {state:?}: {message:?}"
                );
                if state == KernelState::Ready {
                    ready_seen += 1;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("{e:?}"),
            Err(_) => break,
        }
    }
    assert_eq!(ready_seen, 1, "ready after the second start");
    assert_eq!(fx.rt.kernel_state(), KernelState::Ready);
    assert_eq!(
        fx.wb.session_info(&fx.session).await.unwrap().kernel,
        KernelState::Ready
    );
    fx.wb.close_session(&fx.session).await.unwrap();
}

const SLOW_SCRIPT: &str = "import sys, time\nprint('begin', flush=True)\nfor i in range(600):\n    time.sleep(0.1)\nprint('end', flush=True)\n";
const QUICK_SCRIPT: &str = "import sys\nprint('hello from the run', flush=True)\nprint('warn', file=sys.stderr, flush=True)\n";

/// A10, G2, B7 service parts: under an unsandboxed kernel the agent's run asks with an
/// "Unsandboxed run:" card whose id is the tool call id; the card is listed by `session_info`
/// (renderer reload); the run carries its conversation; cancelling the turn cancels the run.
#[tokio::test]
async fn chat_cancel_cancels_the_run_the_turn_started() {
    let fx = fixture_with(fake_kernel).await;
    std::fs::write(fx.rt.repo.join("scripts/slow.py"), SLOW_SCRIPT).unwrap();
    fx.rt.ensure_kernel().await.unwrap();
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call("r1", "run_script", json!({"script": "scripts/slow.py"})),
            stop_tools(),
        ],
        vec![ModelEvent::Text {
            delta: "never".into(),
        }],
    ]));
    fx.rt.set_model_override(Some(model.clone()));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    agent_runner::start_turn(fx.rt.clone(), conv.clone(), "run it", vec![])
        .await
        .unwrap();
    let SessionEvent::Proposal { proposal } =
        next_matching(&mut rx, |e| matches!(e, SessionEvent::Proposal { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(
        proposal.kind,
        analyst_protocol::transcript::ProposalKind::Shell
    );
    assert!(
        proposal.summary.starts_with("Unsandboxed run:"),
        "{}",
        proposal.summary
    );
    assert_eq!(proposal.reason, ProposalReason::Ask);
    assert_eq!(proposal.id, "r1", "the proposal id is the tool call id");
    // A reloaded renderer finds the card and the busy flag in session_info.
    let info = fx.wb.session_info(&fx.session).await.unwrap();
    assert_eq!(info.pending_proposals, vec![proposal.clone()]);
    assert!(info.conversations.iter().any(|c| c.id == conv_id && c.busy));
    fx.rt
        .proposal_respond(&proposal.id, ProposalDecision::Accept { content: None })
        .unwrap();
    assert_eq!(
        fx.rt
            .proposal_respond(&proposal.id, ProposalDecision::Accept { content: None })
            .unwrap_err()
            .code,
        IpcErrorCode::NotFound
    );
    // The run is announced with its conversation.
    let started = next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::RunStarted { run } if run.conversation.is_some()),
    )
    .await;
    let SessionEvent::RunStarted { run } = started else {
        unreachable!()
    };
    assert_eq!(run.conversation.as_deref(), Some(conv_id.as_str()));
    let run_id = run.meta.run_id.clone();
    next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::RunOutput { chunk, .. } if chunk.contains("begin")),
    )
    .await;
    assert!(fx
        .wb
        .session_info(&fx.session)
        .await
        .unwrap()
        .pending_proposals
        .is_empty());

    agent_runner::cancel_turn(&fx.rt, &conv).await;
    assert!(!conv.busy());
    let err = next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnError { .. })).await;
    assert!(matches!(err, SessionEvent::TurnError { message, .. } if message == "cancelled"));
    let done = next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::RunDone { run } if run.meta.run_id == run_id),
    )
    .await;
    let SessionEvent::RunDone { run } = done else {
        unreachable!()
    };
    assert_eq!(
        run.meta.status,
        analyst_protocol::kernel::RunStatus::Cancelled,
        "{run:?}"
    );
    assert_eq!(run.conversation.as_deref(), Some(conv_id.as_str()));
    let listed = fx.rt.runs_list(None);
    assert_eq!(listed[0].conversation.as_deref(), Some(conv_id.as_str()));
    assert_eq!(
        fx.rt.run_get(&run_id).unwrap().view.conversation.as_deref(),
        Some(conv_id.as_str())
    );
    assert_eq!(model.calls(), 1);
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// B4-era finding "run_get after restart": any run in `runs_list` can be read after the
/// session (or the app) was closed and reopened, with its conversation and capped output.
#[tokio::test]
async fn run_get_works_after_restart() {
    let fx = fixture_with(fake_kernel).await;
    std::fs::write(fx.rt.repo.join("scripts/quick.py"), QUICK_SCRIPT).unwrap();
    let mut rx = fx.rt.subscribe();
    let view = fx.rt.run_start("scripts/quick.py", vec![]).await.unwrap();
    assert!(view.conversation.is_none(), "a manual run");
    let run_id = view.meta.run_id.clone();
    next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::RunDone { run } if run.meta.run_id == run_id),
    )
    .await;
    let live = fx.rt.run_get(&run_id).unwrap();
    assert!(live.stdout.contains("hello from the run"), "{live:?}");
    fx.wb.close_session(&fx.session).await.unwrap();

    // A new workbench on the same home: nothing is in memory, and no kernel is running.
    let mut opts = WorkbenchOptions::new(fx.wb.home().to_path_buf());
    opts.autostart_kernel = false;
    let wb = Workbench::new(opts).unwrap();
    wb.open_session(&fx.session).await.unwrap();
    let rt = wb.session(&fx.session).unwrap();
    assert_eq!(rt.runs_in_memory(), 0);
    let listed = rt.runs_list(None);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].meta.run_id, run_id);
    assert_eq!(
        listed[0].meta.status,
        analyst_protocol::kernel::RunStatus::Done
    );
    let detail = rt.run_get(&run_id).unwrap();
    assert!(detail.stdout.contains("hello from the run"), "{detail:?}");
    assert!(detail.stderr.contains("warn"));
    assert!(detail.view.stdout_tail.contains("hello from the run"));
    assert_eq!(detail.view.meta.exit_code, Some(0));
    // Output larger than the cap comes back as its tail, with a note.
    let log = fx.wb.home().join("runs").join(&run_id).join("stdout.log");
    let mut big = "x".repeat(runtime::RUN_GET_MAX_BYTES as usize + 4096);
    big.push_str("\nTHE-END\n");
    std::fs::write(&log, &big).unwrap();
    let detail = rt.run_get(&run_id).unwrap();
    assert!(detail.stdout.starts_with("[... output truncated"));
    assert!(detail.stdout.ends_with("THE-END\n"));
    assert!(detail.stdout.len() <= runtime::RUN_GET_MAX_BYTES as usize + 200);
    assert!(detail.view.stdout_tail.len() <= runtime::TAIL_BYTES);
    wb.close_session(&fx.session).await.unwrap();
}

struct PanickingModel;

impl crate::providers::ModelClient for PanickingModel {
    fn stream(
        &self,
        _req: analyst_protocol::model::ChatRequest,
    ) -> futures::stream::BoxStream<'static, ModelEvent> {
        panic!("boom in the model client")
    }
    fn model_id(&self) -> &str {
        "panic"
    }
}

/// A10: a panic in a turn clears `busy` and reports `turn_error`.
#[tokio::test]
async fn a_panicking_turn_clears_busy_and_reports_turn_error() {
    let fx = fixture().await;
    fx.rt.set_model_override(Some(Arc::new(PanickingModel)));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    let turn = agent_runner::start_turn(fx.rt.clone(), conv.clone(), "hi", vec![])
        .await
        .unwrap();
    let err = next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnError { .. })).await;
    let SessionEvent::TurnError {
        turn: t, message, ..
    } = err
    else {
        unreachable!()
    };
    assert_eq!(t, turn);
    assert!(message.contains("unexpectedly"), "{message}");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!conv.busy());
    let info = fx.rt.info().await;
    assert!(info.conversations.iter().all(|c| !c.busy));
    // The conversation works again.
    let model = Arc::new(ScriptedModelClient::new(vec![vec![ModelEvent::Text {
        delta: "back".into(),
    }]]));
    fx.rt.set_model_override(Some(model));
    agent_runner::start_turn(fx.rt.clone(), conv.clone(), "again", vec![])
        .await
        .unwrap();
    let done = next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;
    assert!(matches!(done, SessionEvent::TurnDone { summary, .. } if summary.text == "back"));
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// A11: a conversation reopened from disk continues with its model history and turn numbers.
#[tokio::test]
async fn reopened_conversation_is_rehydrated() {
    let fx = fixture().await;
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call("p", "pin_fact", json!({"fact": "tau is 2.5 seconds"})),
            stop_tools(),
        ],
        vec![ModelEvent::Text {
            delta: "The first answer.".into(),
        }],
    ]));
    fx.rt.set_model_override(Some(model));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    let first = agent_runner::start_turn(fx.rt.clone(), conv, "remember the decay time", vec![])
        .await
        .unwrap();
    assert_eq!(first, format!("{conv_id}-1"));
    next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;
    fx.wb.close_session(&fx.session).await.unwrap();

    fx.wb.open_session(&fx.session).await.unwrap();
    let rt = fx.wb.session(&fx.session).unwrap();
    let model2 = Arc::new(ScriptedModelClient::new(vec![vec![ModelEvent::Text {
        delta: "Second.".into(),
    }]]));
    rt.set_model_override(Some(model2.clone()));
    let conv = rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = rt.subscribe();
    let second = agent_runner::start_turn(rt.clone(), conv, "and now?", vec![])
        .await
        .unwrap();
    assert_eq!(second, format!("{conv_id}-2"), "turn ids continue");
    next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;
    let req = &model2.recorded()[0];
    let sent = format!("{:?}", req.messages);
    assert!(sent.contains("remember the decay time"), "{sent}");
    assert!(sent.contains("The first answer."), "{sent}");
    assert!(sent.contains("and now?"));
    let all = format!("{req:?}");
    assert!(
        all.contains("tau is 2.5 seconds"),
        "pinned facts survive: {all}"
    );
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// G5 service part: widening the declared scope arrives as an edit-kind card without a path.
#[tokio::test]
async fn scope_widening_is_a_proper_proposal() {
    let fx = fixture().await;
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call("s1", "declare_scope", json!({"files": ["analysis/a.py"]})),
            call(
                "s2",
                "declare_scope",
                json!({"files": ["analysis/a.py", "scripts/b.py", "scripts/c.py"]}),
            ),
            stop_tools(),
        ],
        vec![ModelEvent::Text {
            delta: "done".into(),
        }],
    ]));
    fx.rt.set_model_override(Some(model));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    agent_runner::start_turn(fx.rt.clone(), conv, "widen", vec![])
        .await
        .unwrap();
    let SessionEvent::Proposal { proposal } =
        next_matching(&mut rx, |e| matches!(e, SessionEvent::Proposal { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(proposal.id, "s2");
    assert_eq!(
        proposal.kind,
        analyst_protocol::transcript::ProposalKind::Edit
    );
    assert_eq!(proposal.path, None);
    assert_eq!(
        proposal.summary,
        "Widen edit scope: scripts/b.py, scripts/c.py"
    );
    assert_eq!(proposal.reason, ProposalReason::OutOfScope);
    assert!(!proposal.in_scope);
    assert_eq!(
        fx.rt.pending_proposals(),
        vec![proposal.clone()],
        "listed for a reloaded renderer"
    );
    fx.rt
        .proposal_respond(&proposal.id, ProposalDecision::Accept { content: None })
        .unwrap();
    let r = next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::AgentToolResult { id, .. } if id == "s2"),
    )
    .await;
    assert!(matches!(r, SessionEvent::AgentToolResult { ok: true, .. }));
    next_matching(&mut rx, |e| matches!(e, SessionEvent::TurnDone { .. })).await;
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// IPC.md section 7: `provider_keys_status` and `provider_key_set`.
#[test]
fn provider_keys_status_and_set() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
[[providers]]
name = "ollama"
kind = "openai-compatible"
base_url = "http://localhost:11434/v1"
[[providers]]
name = "acme"
kind = "openai-compatible"
base_url = "https://api.acme.example/v1"
api_key_env = "ANALYST_TEST_ACME_KEY_UNSET"
[[providers]]
name = "envy"
kind = "anthropic"
api_key_env = "ANALYST_TEST_ENVY_KEY"
"#,
    )
    .unwrap();
    std::env::set_var("ANALYST_TEST_ENVY_KEY", "sk-env");
    let wb = Workbench::new(WorkbenchOptions::new(home.path().to_path_buf())).unwrap();
    let rows = wb.provider_keys_status();
    assert_eq!(rows.len(), 3, "one row per configured provider");
    let row = |rows: &[ProviderKeyStatus], n: &str| rows.iter().find(|r| r.provider == n).cloned();
    let acme = row(&rows, "acme").unwrap();
    assert!(!acme.has_key);
    assert_eq!(acme.source, None);
    assert_eq!(
        acme.api_key_env.as_deref(),
        Some("ANALYST_TEST_ACME_KEY_UNSET")
    );
    assert_eq!(row(&rows, "envy").unwrap().source.as_deref(), Some("env"));
    let ollama = row(&rows, "ollama").unwrap();
    assert_eq!(ollama.source, None);
    // Without `api_key_env` the variable name is derived from the provider name.
    assert_eq!(ollama.api_key_env.as_deref(), Some("OLLAMA_API_KEY"));

    wb.provider_key_set("acme", "sk-stored").unwrap();
    let acme = row(&wb.provider_keys_status(), "acme").unwrap();
    assert!(acme.has_key);
    assert_eq!(acme.source.as_deref(), Some("stored"));
    // Stored in the home's `.env` under the provider's variable name; nothing else.
    let creds = home.path().join(".env");
    assert!(std::fs::read_to_string(&creds)
        .unwrap()
        .contains("ANALYST_TEST_ACME_KEY_UNSET=\"sk-stored\""));
    assert!(!home.path().join("credentials.toml").exists());
    wb.provider_key_set("ollama", "sk-derived").unwrap();
    assert!(std::fs::read_to_string(&creds)
        .unwrap()
        .contains("OLLAMA_API_KEY=\"sk-derived\""));
    assert_eq!(
        row(&wb.provider_keys_status(), "ollama")
            .unwrap()
            .source
            .as_deref(),
        Some("stored")
    );
    wb.provider_key_set("ollama", "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&creds).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // The environment variable wins over a stored key.
    wb.provider_key_set("envy", "sk-other").unwrap();
    assert_eq!(
        row(&wb.provider_keys_status(), "envy")
            .unwrap()
            .source
            .as_deref(),
        Some("env")
    );
    // An empty key removes the stored one.
    wb.provider_key_set("acme", "").unwrap();
    assert_eq!(
        row(&wb.provider_keys_status(), "acme").unwrap().source,
        None
    );
    assert_eq!(
        wb.provider_key_set("nope", "x").unwrap_err().code,
        IpcErrorCode::NotFound
    );
    // No value is ever part of a status row.
    assert!(!format!("{:?}", wb.provider_keys_status()).contains("sk-"));
}

/// U11: kernels are spawned without provider keys and with the widened tool PATH.
#[tokio::test]
async fn kernel_processes_do_not_inherit_provider_keys() {
    use tokio::io::AsyncReadExt;
    std::env::set_var("ANALYST_TEST_KERNEL_KEY", "sk-kernel-secret");
    std::env::set_var("OPENROUTER_API_KEY", "sk-openrouter-secret");
    let line: Vec<String> = vec![
        "sh".into(),
        "-c".into(),
        "printf '%s' \"custom=[$ANALYST_TEST_KERNEL_KEY] common=[$OPENROUTER_API_KEY] path=[$PATH]\"".into(),
    ];
    let mut k = crate::kernel::transport::spawn_line_without_env(
        &line,
        &["ANALYST_TEST_KERNEL_KEY".to_string()],
    )
    .unwrap();
    let mut out = String::new();
    k.stdout.read_to_string(&mut out).await.unwrap();
    assert!(out.contains("custom=[]"), "{out}");
    assert!(out.contains("common=[]"), "{out}");
    assert!(!out.contains("secret"), "{out}");
    assert!(!out.contains("path=[]"), "{out}");
    // The names come from the configured providers plus the common ones.
    let mut global = GlobalConfig::default();
    global
        .providers
        .push(analyst_protocol::config::ProviderConfig {
            name: "x".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: None,
            api_key_env: Some("MY_CUSTOM_KEY".into()),
        });
    let names = terminal::key_env_names(&global);
    assert!(names.contains(&"MY_CUSTOM_KEY".to_string()));
    // The derived name is scrubbed too, in case the key is exported under it.
    assert!(names.contains(&"X_API_KEY".to_string()));
    assert!(names.contains(&"ANTHROPIC_API_KEY".to_string()));
    std::env::remove_var("OPENROUTER_API_KEY");
}

/// B12: when the tooling environment cannot be prepared, the kernel reports a clear error
/// state instead of a spawn failure; while it is being prepared, starts wait for it.
#[tokio::test]
async fn kernel_start_waits_for_tooling_and_reports_its_failure() {
    let fx = fixture().await;
    let mut rx = fx.rt.subscribe();
    fx.wb.tooling.set(ToolingState::Preparing);
    let rt = fx.rt.clone();
    let start = tokio::spawn(async move { rt.ensure_kernel().await.map(|_| ()) });
    let e = next_matching(&mut rx, |e| {
        matches!(
            e,
            SessionEvent::KernelState {
                message: Some(_),
                ..
            }
        )
    })
    .await;
    assert!(
        matches!(&e, SessionEvent::KernelState { state: KernelState::Starting, message: Some(m), .. } if m.contains("tooling")),
        "{e:?}"
    );
    assert!(!start.is_finished());
    let msg = "`uv` was not found (looked on PATH, in ~/.local/bin)";
    fx.wb.tooling.set(ToolingState::Failed(msg.into()));
    let err = start.await.unwrap().expect_err("the start must fail");
    assert_eq!(err.code, IpcErrorCode::KernelOffline);
    let e = next_matching(&mut rx, |e| {
        matches!(
            e,
            SessionEvent::KernelState {
                state: KernelState::Error,
                ..
            }
        )
    })
    .await;
    assert!(
        matches!(&e, SessionEvent::KernelState { message: Some(m), .. } if m.contains("uv") && m.contains("looked")),
        "{e:?}"
    );
    assert_eq!(fx.rt.kernel_state(), KernelState::Error);
    fx.wb.tooling.set(ToolingState::Ready);
    fx.wb.close_session(&fx.session).await.unwrap();
}

/// B7 service part: run records in memory are bounded; evicted runs stay listed and readable.
#[tokio::test]
async fn finished_runs_in_memory_are_bounded() {
    let fx = fixture().await;
    let session = fx.rt.session_snapshot();
    let total = runtime::MAX_FINISHED_RUNS + 60;
    for i in 0..total {
        let meta = analyst_protocol::kernel::RunMeta {
            run_id: format!("20260101-{i:06}-bbbbbb"),
            script: "scripts/x.py".into(),
            args: vec![],
            status: analyst_protocol::kernel::RunStatus::Done,
            exit_code: Some(0),
            started_at: format!("2026-01-01T00:{:02}:{:02}Z", i / 60, i % 60),
            finished_at: None,
            out_dir: String::new(),
            sandbox: "none".into(),
            tree_hash: None,
            pid: None,
            host: "local".into(),
        };
        session.append_run(&meta).unwrap();
        fx.rt.insert_run(runtime::RunRecord {
            meta,
            target: "local".into(),
            stdout: String::new(),
            stderr: String::new(),
            artifacts: vec![],
            source: runtime::RunSource::Kernel,
            finished: true,
            conversation: None,
        });
    }
    assert_eq!(fx.rt.runs_in_memory(), runtime::MAX_FINISHED_RUNS);
    let listed = fx.rt.runs_list(Some(1000));
    assert_eq!(listed.len(), total, "evicted runs are still listed");
    assert!(
        fx.rt.run_get("20260101-000000-bbbbbb").is_ok(),
        "and readable"
    );
    fx.wb.close_session(&fx.session).await.unwrap();
}
