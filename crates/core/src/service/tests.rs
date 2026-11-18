//! Service-level unit tests with the scripted model client (no network; the tests that need a
//! kernel use the stdlib-only fake in `tests/fake_kernel.py`).

use super::*;
use analyst_protocol::transcript::Decision;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::broadcast;
use wb_providers::model::{ModelEvent, StopReason};
use wb_providers::ScriptedModelClient;

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
            wb_providers::model::ContentPart::ToolResult {
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

impl wb_providers::ModelClient for PanickingModel {
    fn stream(
        &self,
        _req: wb_providers::model::ChatRequest,
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
        fx.rt.runs.insert(super::runs::RunRecord {
            meta,
            target: "local".into(),
            stdout: String::new(),
            stderr: String::new(),
            artifacts: vec![],
            source: super::runs::RunSource::Kernel,
            finished: true,
            completed: true,
            conversation: None,
            duration_s: None,
            pull_error: None,
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

/// One run service: a run started by the UI (`run_start`) and a run started by the agent
/// (`run.script`) of the same script produce the same session events, in the same order.
/// The only difference is the conversation the agent's run is attributed to, and that is
/// part of its first (and only) `run_started`.
#[tokio::test]
async fn agent_and_ui_runs_produce_the_same_events() {
    /// The run events of one run, with the run id and timestamps left out.
    fn shape(events: &[SessionEvent], run_id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut output = String::new();
        for e in events {
            match e {
                SessionEvent::RunStarted { run } if run.meta.run_id == run_id => out.push(format!(
                    "started script={} args={:?} status={:?} target={}",
                    run.meta.script, run.meta.args, run.meta.status, run.target
                )),
                SessionEvent::RunOutput {
                    run_id: id, chunk, ..
                } if id == run_id => output.push_str(chunk),
                SessionEvent::RunArtifact {
                    run_id: id,
                    artifact,
                    ..
                } if id == run_id => out.push(format!(
                    "artifact kind={:?} name={}",
                    artifact.kind,
                    artifact.path.rsplit('/').next().unwrap_or("")
                )),
                SessionEvent::RunDone { run } if run.meta.run_id == run_id => out.push(format!(
                    "done status={:?} exit={:?} artifacts={} stdout={:?}",
                    run.meta.status,
                    run.meta.exit_code,
                    run.artifacts.len(),
                    run.stdout_tail
                )),
                _ => {}
            }
        }
        out.push(format!("output={output:?}"));
        out
    }
    async fn collect_until_done(
        rx: &mut Subscription,
        run_id: &mut Option<String>,
    ) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let e = match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(e)) => e,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                other => panic!("run events not received: {other:?}"),
            };
            if let SessionEvent::RunStarted { run } = &e {
                run_id.get_or_insert_with(|| run.meta.run_id.clone());
            }
            let done = matches!(&e, SessionEvent::RunDone { run } if Some(&run.meta.run_id) == run_id.as_ref());
            events.push(e);
            if done {
                return events;
            }
        }
    }

    let fx = fixture_with(fake_kernel).await;
    std::fs::write(
        fx.rt.repo.join("scripts/hello.py"),
        "import sys\nprint('hello', *sys.argv[1:], flush=True)\n",
    )
    .unwrap();
    fx.rt.ensure_kernel().await.unwrap();

    // The UI's Run button.
    let mut rx = fx.rt.subscribe();
    let view = fx
        .rt
        .run_start("scripts/hello.py", vec!["--n".into(), "1".into()])
        .await
        .unwrap();
    assert!(view.conversation.is_none());
    let mut ui_id = Some(view.meta.run_id.clone());
    let ui_events = collect_until_done(&mut rx, &mut ui_id).await;

    // The agent's `run.script`, through the same run service.
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let mut ctx = crate::tools::ToolContext::new(
        &fx.rt.repo,
        &fx.rt.home,
        fx.rt.session_snapshot(),
        &conv_id,
        fx.rt.project(),
    );
    ctx.use_runs(fx.rt.runs.clone());
    ctx.kernel = fx.rt.kernel_client().await;
    let mut rx = fx.rt.subscribe();
    let result = ctx
        .dispatch(
            "t1",
            "run.script",
            &json!({"script": "scripts/hello.py", "args": ["--n", "1"]}),
        )
        .await
        .unwrap();
    let agent_run = result["run_id"].as_str().unwrap().to_string();
    assert_eq!(result["status"], "done", "{result}");
    assert!(result["stdout"].as_str().unwrap().contains("hello --n 1"));
    let mut agent_id = None;
    let agent_events = collect_until_done(&mut rx, &mut agent_id).await;
    assert_eq!(agent_id.as_deref(), Some(agent_run.as_str()));

    let ui_shape = shape(&ui_events, ui_id.as_deref().unwrap());
    let agent_shape = shape(&agent_events, &agent_run);
    assert_eq!(ui_shape, agent_shape);
    assert!(ui_shape[0].starts_with("started "), "{ui_shape:?}");
    assert!(ui_shape.iter().any(|l| l.starts_with("done status=Done")));
    // Exactly one `run_started` and one `run_done` per run, and the agent's run carries its
    // conversation from the start.
    for (events, id, conv) in [
        (&ui_events, ui_id.as_deref().unwrap(), None),
        (&agent_events, agent_run.as_str(), Some(conv_id.as_str())),
    ] {
        let started: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::RunStarted { run } if run.meta.run_id == id => Some(run),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].conversation.as_deref(), conv);
        let done = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::RunDone { run } if run.meta.run_id == id))
            .count();
        assert_eq!(done, 1);
    }
    // Both are in the one run history.
    let listed: Vec<String> = fx
        .rt
        .runs_list(None)
        .into_iter()
        .map(|r| r.meta.run_id)
        .collect();
    assert!(listed.contains(&agent_run) && listed.contains(ui_id.as_ref().unwrap()));
    fx.wb.shutdown().await;
}

fn deck_template() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../present/evals/templates/plain/plain.pptx")
}

/// The deck kernel needs its Python environment (`present/python/kernel/.venv`); a checkout
/// without it skips the deck tests.
fn deck_runtime_available(home: &std::path::Path) -> bool {
    let ok = analyst_deck::kernel::resolve_runtime_python(home, None).is_some();
    if !ok {
        eprintln!("skipped: no deck kernel interpreter (run `uv sync` in present/python/kernel)");
    }
    ok
}

fn deck_options(opts: &mut WorkbenchOptions) {
    fake_kernel(opts);
    opts.deck.engine = "none".into();
    opts.deck.kernel_env = vec![(
        "PRESENT_FONT_DIRS".into(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../present/vendor/fonts")
            .to_string_lossy()
            .into_owned(),
    )];
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

fn text(t: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Text { delta: t.into() },
        ModelEvent::Stop {
            reason: StopReason::End,
            message: None,
        },
    ]
}

/// One control loop over both workbenches: a single turn declares one scope for a file and
/// a new slide, writes a script, runs it, and proposes a slide; one checkpoint covers the
/// whole turn, and one undo takes back the script and the slide together.
#[tokio::test]
async fn one_turn_changes_code_runs_it_and_adds_a_slide_and_one_undo_reverts_all() {
    let fx = fixture_with(deck_options).await;
    if !deck_runtime_available(&fx.rt.home) {
        return;
    }
    // A deck is created on request, from a template file; nothing starts.
    assert!(fx.rt.decks_list().is_empty());
    let deck = fx
        .rt
        .deck_create(None, "Q3 Review", &deck_template().to_string_lossy(), false)
        .unwrap();
    assert_eq!(
        (deck.slug.as_str(), deck.slides, deck.open),
        ("q3-review", 0, false)
    );
    assert!(fx.rt.repo.join("decks/q3-review/deck/deck.json").is_file());
    assert!(fx
        .rt
        .repo
        .join("decks/q3-review/template/plain.pptx")
        .is_file());
    // The project template ignores deck exports and the deck kernel's caches.
    let ignore = std::fs::read_to_string(fx.rt.repo.join(".gitignore")).unwrap();
    assert!(ignore.contains("decks/*/exports/") && ignore.contains("decks/*/.present/cache/"));
    // The user commits; from here on the turn's changes are what `git status` shows.
    crate::checkpoint::git(&fx.rt.repo, &["add", "-A"], None).unwrap();
    crate::checkpoint::git(&fx.rt.repo, &["commit", "-q", "-m", "deck"], None).unwrap();
    let deck_json_before =
        std::fs::read_to_string(fx.rt.repo.join("decks/q3-review/deck/deck.json")).unwrap();

    // Auto within the declared scope; the fake kernel has no sandbox, so the run asks.
    fx.rt
        .set_policy(&PolicyPatch {
            preset: Some(analyst_protocol::config::PolicyPreset::Auto),
            ..Default::default()
        })
        .unwrap();
    fx.rt.ensure_kernel().await.unwrap();
    let script = "import sys\nprint('margin', 18, flush=True)\n";
    let model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call(
                "s1",
                "declare_scope",
                json!({"files": ["scripts/margin.py"], "new_slides": 1, "deck": "q3-review", "plan": "compute the margin and show it"}),
            ),
            call(
                "e1",
                "fs_propose_create",
                json!({"path": "scripts/margin.py", "content": script}),
            ),
            call("r1", "run_script", json!({"script": "scripts/margin.py"})),
            call(
                "d1",
                "slide_propose_create",
                json!({"deck": "q3-review", "slide": slide_doc("Margin recovered to 18%")}),
            ),
            stop_tools(),
        ],
        text("Done: one script, one run, one slide."),
    ]));
    fx.rt.set_model_override(Some(model.clone()));
    let conv_id = fx.rt.meta().active_conversation.unwrap();
    let conv = fx.rt.conversation_runtime(&conv_id).unwrap();
    let mut rx = fx.rt.subscribe();
    let turn = agent_runner::start_turn(fx.rt.clone(), conv.clone(), "margin slide", vec![])
        .await
        .unwrap();
    // The unsandboxed run is the only thing that asks.
    let SessionEvent::Proposal { proposal } =
        next_matching(&mut rx, |e| matches!(e, SessionEvent::Proposal { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(proposal.id, "r1");
    fx.rt
        .proposal_respond(&proposal.id, ProposalDecision::Accept { content: None })
        .unwrap();
    let done = next_matching(&mut rx, |e| {
        matches!(
            e,
            SessionEvent::TurnDone { .. } | SessionEvent::TurnError { .. }
        )
    })
    .await;
    let SessionEvent::TurnDone { summary, .. } = done else {
        panic!("the turn failed: {done:?}")
    };
    // The turn changed a script and a slide, and ran the script.
    assert_eq!(summary.runs.len(), 1);
    assert_eq!(
        summary.files_changed.len(),
        2,
        "{:?}",
        summary.files_changed
    );
    assert!(summary
        .files_changed
        .contains(&"scripts/margin.py".to_string()));
    let slide_file = summary
        .files_changed
        .iter()
        .find(|f| f.starts_with("decks/q3-review/deck/slides/"))
        .expect("the slide is a file of the repository")
        .clone();
    assert!(fx.rt.repo.join(&slide_file).is_file());
    assert_eq!(fx.rt.decks_list()[0].slides, 1);
    assert!(
        fx.rt.decks_list()[0].open,
        "the deck kernel started on first use"
    );

    // One transcript: one checkpoint, the run, and the slide proposal with its shape.
    let events = fx.rt.conversation_transcript(&conv_id).unwrap();
    let checkpoints = events
        .iter()
        .filter(|e| matches!(e, analyst_protocol::transcript::Event::Checkpoint { .. }))
        .count();
    assert_eq!(checkpoints, 1, "one checkpoint covers the whole turn");
    assert!(events
        .iter()
        .any(|e| matches!(e, analyst_protocol::transcript::Event::Run { .. })));
    let slide_proposal = events
        .iter()
        .find_map(|e| match e {
            analyst_protocol::transcript::Event::Proposal {
                kind: analyst_protocol::transcript::ProposalKind::SlideCreate,
                deck,
                targets,
                before,
                after,
                ..
            } => Some((deck.clone(), targets.clone(), before.clone(), after.clone())),
            _ => None,
        })
        .expect("the slide proposal is in the transcript");
    assert_eq!(slide_proposal.0.as_deref(), Some("q3-review"));
    assert_eq!(slide_proposal.1, Some(vec![slide_file.clone()]));
    assert!(slide_proposal.2.is_none(), "a new slide has no before");
    assert_eq!(
        slide_proposal.3.as_ref().unwrap()["layout"],
        "Title and Content"
    );
    // The new slide was announced with `new_slides`, so the policy applied it on its own.
    assert!(events.iter().any(|e| matches!(
        e,
        analyst_protocol::transcript::Event::Decision { proposal_id, decision: Decision::Auto, .. }
            if proposal_id == "d1"
    )));

    // One undo timeline: the turn's checkpoint is listed with its turn, and reverting the
    // turn takes back the script and the slide together.
    let history = fx.rt.history_list(None).unwrap();
    assert!(history
        .iter()
        .any(|c| c.turn.as_deref() == Some(turn.as_str())));
    fx.rt.history_revert_turn(&turn).await.unwrap();
    assert!(!fx.rt.repo.join("scripts/margin.py").exists());
    assert!(!fx.rt.repo.join(&slide_file).exists());
    assert_eq!(
        std::fs::read_to_string(fx.rt.repo.join("decks/q3-review/deck/deck.json")).unwrap(),
        deck_json_before
    );
    assert_eq!(fx.rt.decks_list()[0].slides, 0);
    let status = crate::checkpoint::git(&fx.rt.repo, &["status", "--porcelain"], None).unwrap();
    assert_eq!(
        status.trim(),
        "",
        "the working tree is as it was before the turn"
    );
    // The restore itself can be undone: the state it replaced was checkpointed.
    let history = fx.rt.history_list(None).unwrap();
    let safety = history
        .iter()
        .find(|c| {
            c.label
                .as_deref()
                .is_some_and(|l| l.starts_with("before restoring"))
        })
        .expect("a checkpoint of the state the restore replaced");
    fx.rt.history_restore(&safety.id).await.unwrap();
    assert!(fx.rt.repo.join("scripts/margin.py").is_file());
    assert!(fx.rt.repo.join(&slide_file).is_file());
    fx.wb.shutdown().await;
}

/// Modes are permission profiles over the one loop, enforced by the policy's mode rules:
/// slides mode offers the deck tools and no code tools; polish may change and reorder
/// slides but not add slides, citations, or elements; a locked slide is never changed.
/// A slide proposal keeps its before/after shape for the user's decision.
#[tokio::test]
async fn deck_modes_are_enforced_by_policy_and_slide_proposals_keep_their_shape() {
    use crate::agent::{Agent, AgentConfig, ApprovalDecision, ApprovalRequest, Approver};
    use crate::tools::gate::Policy;
    use analyst_protocol::evals::Mode;
    use analyst_protocol::transcript::{Event, ProposalKind};

    /// Accepts everything; edits the first slide proposal before accepting it.
    struct EditingApprover {
        seen: std::sync::Mutex<Vec<ApprovalRequest>>,
    }
    #[async_trait::async_trait]
    impl Approver for EditingApprover {
        async fn approve(&self, _req: &ApprovalRequest) -> bool {
            true
        }
        async fn decide(&self, req: &ApprovalRequest) -> ApprovalDecision {
            let first = {
                let mut seen = self.seen.lock().unwrap();
                seen.push(req.clone());
                seen.len() == 1
            };
            let content = match (&req.deck, first) {
                (Some(d), true) => {
                    let mut slide = d.after.clone().unwrap();
                    slide["placeholders"]["title"]["text"]["paragraphs"][0]["runs"][0]["t"] =
                        json!("Edited by the user");
                    Some(slide.to_string())
                }
                _ => None,
            };
            ApprovalDecision::Accept { content }
        }
    }

    let fx = fixture_with(deck_options).await;
    if !deck_runtime_available(&fx.rt.home) {
        return;
    }
    fx.rt
        .deck_create(
            Some("board"),
            "Board",
            &deck_template().to_string_lossy(),
            false,
        )
        .unwrap();
    let cited = {
        let mut s = slide_doc("Now with a claim");
        s["placeholders"]["body"]["text"]["paragraphs"][0]["runs"][0]["cite"] =
            json!(["ref:r-12345678@abcdef123456#p1"]);
        s
    };
    let model = Arc::new(ScriptedModelClient::new(vec![
        // Turn 1, slides mode, review preset.
        vec![
            call(
                "a1",
                "slide_propose_create",
                json!({"slide": slide_doc("Proposed title")}),
            ),
            call(
                "a2",
                "fs_propose_create",
                json!({"path": "scripts/x.py", "content": "x = 1\n"}),
            ),
            call("a3", "run_script", json!({"script": "scripts/x.py"})),
            stop_tools(),
        ],
        text("slide added"),
    ]));
    let project = fx.rt.project();
    let conv = "c-modes";
    let mut ctx = crate::tools::ToolContext::new(
        &fx.rt.repo,
        &fx.rt.home,
        fx.rt.session_snapshot(),
        conv,
        project.clone(),
    );
    ctx.use_runs(fx.rt.runs.clone());
    ctx.decks = fx.rt.decks.clone();
    let approver = Arc::new(EditingApprover {
        seen: Default::default(),
    });
    let policy = Policy::for_project(&fx.rt.repo, &project, Mode::Slides).unwrap();
    let config = AgentConfig::new(Mode::Slides, "scripted/model", Default::default(), 10);
    let mut agent = Agent::new(model, ctx, policy, approver.clone(), config);
    let session = fx.rt.session_snapshot();
    let results = |from: usize| -> std::collections::HashMap<String, (bool, Value)> {
        session
            .read_transcript(conv)
            .unwrap()
            .into_iter()
            .skip(from)
            .filter_map(|e| match e {
                Event::ToolResult { id, ok, result, .. } => Some((id, (ok, result))),
                _ => None,
            })
            .collect()
    };

    // Turn 1: the deck tool is offered and reviewed; code tools are not part of the mode.
    let s1 = agent.run_turn("add a slide").await.unwrap();
    let r = results(0);
    assert!(r["a1"].0, "{:?}", r["a1"]);
    assert_eq!(r["a1"].1["user_modified"], true);
    for id in ["a2", "a3"] {
        assert!(!r[id].0);
        assert!(
            r[id].1["error"]
                .as_str()
                .unwrap()
                .contains("not available in slides mode"),
            "{:?}",
            r[id]
        );
    }
    assert_eq!(s1.files_changed.len(), 1);
    let slide_file = s1.files_changed[0].clone();
    let slide_id = slide_file
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches(".json")
        .to_string();
    // The request the user saw keeps the proposal's shape, and what they edited was written.
    let seen = approver.seen.lock().unwrap().clone();
    let d = seen[0].deck.as_ref().expect("a deck proposal");
    assert_eq!(
        (d.deck.as_str(), d.kind),
        ("board", ProposalKind::SlideCreate)
    );
    assert!(d.before.is_none() && d.after.as_ref().unwrap()["layout"] == "Title and Content");
    assert_eq!(d.targets, vec![slide_file.clone()]);
    let view = agent_runner::proposal_view(&fx.rt.repo, "a1", conv, &seen[0]);
    assert_eq!(view.kind, ProposalKind::SlideCreate);
    assert_eq!(view.deck.as_deref(), Some("board"));
    assert_eq!(view.slide_ids, Some(vec![slide_id.clone()]));
    assert!(view.before.is_none() && view.after.is_some() && view.proposed.is_some());
    assert!(std::fs::read_to_string(fx.rt.repo.join(&slide_file))
        .unwrap()
        .contains("Edited by the user"));

    // Turn 2: polish. The scripted calls name the slide the first turn created.
    agent.model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call(
                "b1",
                "slide_propose_create",
                json!({"slide": slide_doc("Another")}),
            ),
            call(
                "b2",
                "slide_propose_update",
                json!({"slide_id": slide_id, "slide": cited}),
            ),
            // A patch keeps the element ids, so nothing new is added.
            call(
                "b3",
                "slide_propose_update",
                json!({"slide_id": slide_id, "patch": [{"op": "replace", "path": "/placeholders/title/text/paragraphs/0/runs/0/t", "value": "Sharper title"}]}),
            ),
            call("b4", "slide_propose_delete", json!({"slide_id": slide_id})),
            stop_tools(),
        ],
        text("polished"),
    ]));
    agent.set_mode(Mode::Polish);
    let before = session.read_transcript(conv).unwrap().len();
    agent.run_turn("polish it").await.unwrap();
    let r = results(before);
    assert!(r["b1"].1["error"]
        .as_str()
        .unwrap()
        .contains("not available in polish mode"));
    let new_claim = r["b2"].1["error"].as_str().unwrap();
    assert!(
        new_claim.contains("polish mode does not allow `new-citation`"),
        "{new_claim}"
    );
    assert!(
        r["b3"].0,
        "a wording change is what polish is for: {:?}",
        r["b3"]
    );
    assert!(r["b4"].1["error"]
        .as_str()
        .unwrap()
        .contains("not available in polish mode"));
    assert!(std::fs::read_to_string(fx.rt.repo.join(&slide_file))
        .unwrap()
        .contains("Sharper title"));
    // The update proposal carries the slide before and after.
    let update = session
        .read_transcript(conv)
        .unwrap()
        .into_iter()
        .find_map(|e| match e {
            Event::Proposal {
                id,
                kind: ProposalKind::SlideUpdate,
                before,
                after,
                ..
            } if id == "b3" => Some((before, after)),
            _ => None,
        })
        .unwrap();
    assert!(update.0.unwrap().to_string().contains("Edited by the user"));
    assert!(update.1.unwrap().to_string().contains("Sharper title"));

    // Turn 3: the user locks the slide (a UI command of the deck service); the loop
    // refuses to change it in any mode.
    fx.rt
        .deck("board")
        .await
        .unwrap()
        .slide_lock(&slide_id, None, true)
        .unwrap();
    agent.model = Arc::new(ScriptedModelClient::new(vec![
        vec![
            call(
                "c1",
                "slide_propose_update",
                json!({"slide_id": slide_id, "slide": slide_doc("Changed while locked")}),
            ),
            stop_tools(),
        ],
        text("could not"),
    ]));
    agent.set_mode(Mode::Slides);
    let before = session.read_transcript(conv).unwrap().len();
    agent.run_turn("change it").await.unwrap();
    let r = results(before);
    assert!(!r["c1"].0);
    assert!(
        r["c1"].1["error"].as_str().unwrap().contains("locked"),
        "{:?}",
        r["c1"]
    );
    assert_eq!(r["c1"].1["denied"], "locked");
    fx.wb.shutdown().await;
}

/// Slides cite analysis results directly: the artifact index lists what runs produced, a
/// figure is placed on a slide by run id and artifact path, the image is copied into the
/// deck (data is not), the element carries the run's citation, and the checker reports the
/// element as stale once the code has changed. The scan-and-import bridge is gone.
#[tokio::test]
async fn a_slide_cites_an_analysis_run_and_goes_stale_when_the_code_changes() {
    let fx = fixture_with(deck_options).await;
    if !deck_runtime_available(&fx.rt.home) {
        return;
    }
    fx.rt
        .deck_create(Some("q3"), "Q3", &deck_template().to_string_lossy(), false)
        .unwrap();
    // A script that writes a figure (a 1x1 PNG) and a data file into its output directory.
    let script = r#"import base64, os, sys
out = os.environ["ANALYST_OUT_DIR"]
os.makedirs(out, exist_ok=True)
png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
open(os.path.join(out, "margin.png"), "wb").write(base64.b64decode(png))
open(os.path.join(out, "margin.csv"), "w").write("region,margin\nnorth,0.18\n")
print("wrote", *sys.argv[1:])
"#;
    std::fs::write(fx.rt.repo.join("scripts/margin.py"), script).unwrap();
    fx.rt.ensure_kernel().await.unwrap();
    let mut rx = fx.rt.subscribe();
    let run = fx
        .rt
        .run_start("scripts/margin.py", vec!["--region".into(), "north".into()])
        .await
        .unwrap();
    let run_id = run.meta.run_id.clone();
    next_matching(
        &mut rx,
        |e| matches!(e, SessionEvent::RunDone { run } if run.meta.run_id == run_id),
    )
    .await;

    // The artifact index: from the run record and the kernel's artifact list.
    let all = fx.rt.artifacts_list(None, None, None);
    assert_eq!(all.len(), 2, "{all:?}");
    let figures = fx.rt.artifacts_list(
        Some(&run_id),
        Some(analyst_protocol::kernel::ArtifactKind::Figure),
        None,
    );
    assert_eq!(figures.len(), 1);
    let fig = &figures[0];
    assert!(
        fig.path.starts_with("out/") && fig.path.ends_with("/margin.png"),
        "{}",
        fig.path
    );
    assert_eq!(fig.script, "scripts/margin.py");
    assert_eq!(fig.args, vec!["--region", "north"]);
    assert!(fig.tree_hash.is_some() && !fig.stale);

    // The agent lists artifacts and places the figure by run id and path.
    let deck = fx.rt.deck("q3").await.unwrap();
    let slide = deck.slide_create(None, "Title and Content").await.unwrap();
    let slide_id = slide.slide.id.to_string();
    let mut ctx = crate::tools::ToolContext::new(
        &fx.rt.repo,
        &fx.rt.home,
        fx.rt.session_snapshot(),
        "c-cite",
        fx.rt.project(),
    );
    ctx.use_runs(fx.rt.runs.clone());
    ctx.decks = fx.rt.decks.clone();
    let listed = ctx
        .dispatch("t0", "artifacts.list", &json!({"kind": "figure"}))
        .await
        .unwrap();
    assert_eq!(listed["artifacts"][0]["run_id"], run_id.as_str());
    // Data is never copied into a deck.
    let csv = all.iter().find(|a| a.path.ends_with(".csv")).unwrap();
    let refused = ctx
        .dispatch(
            "t1",
            "slide.place_figure",
            &json!({"slide_id": slide_id, "run_id": run_id, "artifact": csv.path, "placeholder": "body"}),
        )
        .await
        .unwrap_err();
    assert!(
        refused.message.contains("only figures are placed"),
        "{}",
        refused.message
    );
    let placed = ctx
        .dispatch(
            "t2",
            "slide.place_figure",
            &json!({"slide_id": slide_id, "run_id": run_id, "artifact": fig.path, "placeholder": "body", "alt": "Margin by region"}),
        )
        .await
        .unwrap();
    assert_eq!(placed["updated"], slide_id.as_str(), "{placed}");
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(
            fx.rt
                .repo
                .join(crate::service::decks::slide_rel("q3", &slide_id)),
        )
        .unwrap(),
    )
    .unwrap();
    let image = &doc["placeholders"]["body"];
    assert_eq!(image["type"], "image");
    let cite = &image["run_cite"];
    assert_eq!(cite["run_id"], run_id.as_str());
    assert_eq!(cite["artifact"], fig.path.as_str());
    assert_eq!(cite["script"], "scripts/margin.py");
    assert_eq!(cite["args"], json!(["--region", "north"]));
    assert_eq!(cite["tree_hash"], fig.tree_hash.clone().unwrap().as_str());
    // The image is in the deck's assets, named by its content; nothing else was copied.
    let asset = image["asset"].as_str().unwrap();
    assert!(
        asset.starts_with("assets/") && asset.ends_with(".png"),
        "{asset}"
    );
    let assets: Vec<String> = std::fs::read_dir(fx.rt.repo.join("decks/q3/assets"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        assets,
        vec![asset.trim_start_matches("assets/").to_string()]
    );

    // The UI places a figure as a direct user action (`slide_place_figure`): the same core
    // function, written at once against the slide's hash, the placeholder chosen from the
    // layout, the same content-addressed asset, the same run citation.
    let other = deck
        .slide_create(Some(&slide_id), "Title and Content")
        .await
        .unwrap();
    let other_id = other.slide.id.to_string();
    let view = fx
        .rt
        .slide_place_figure("q3", &other_id, &run_id, &fig.path, None)
        .await
        .unwrap();
    let placed_doc = serde_json::to_value(&view.slide).unwrap();
    let (_, ui_image) = placed_doc["placeholders"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, v)| v["type"] == "image")
        .expect("an image placeholder was filled");
    assert_eq!(ui_image["asset"], asset);
    assert_eq!(ui_image["run_cite"]["run_id"], run_id.as_str());
    assert_eq!(ui_image["run_cite"]["artifact"], fig.path.as_str());
    assert_eq!(ui_image["run_cite"]["args"], json!(["--region", "north"]));
    assert!(ui_image["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("e-")));
    let on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(
            fx.rt
                .repo
                .join(crate::service::decks::slide_rel("q3", &other_id)),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk["placeholders"], placed_doc["placeholders"]);
    // Data is refused here too.
    let err = fx
        .rt
        .slide_place_figure("q3", &other_id, &run_id, &csv.path, Some("body"))
        .await
        .unwrap_err();
    assert_eq!(err.code, IpcErrorCode::Invalid, "{err}");
    deck.slide_delete(&other_id).unwrap();

    // The checker: not stale while the code is the code the run executed (changing the
    // deck does not change that) ...
    let stale = |diags: &[Value]| -> Vec<Value> {
        diags
            .iter()
            .filter(|d| d["rule"] == "stale")
            .cloned()
            .collect()
    };
    let before = deck.run_checks(None).await.unwrap();
    assert!(stale(&before).is_empty(), "{before:?}");
    // ... and a warning on that element once the script has changed.
    std::fs::write(
        fx.rt.repo.join("scripts/margin.py"),
        script.replace("0.18", "0.21"),
    )
    .unwrap();
    let after = deck.run_checks(None).await.unwrap();
    let found = stale(&after);
    assert_eq!(found.len(), 1, "{after:?}");
    assert_eq!(found[0]["severity"], "warn");
    assert_eq!(found[0]["slide_id"], slide_id.as_str());
    assert_eq!(found[0]["element_id"], image["id"]);
    assert!(found[0]["message"].as_str().unwrap().contains(&run_id));
    assert!(fx.rt.artifacts_list(Some(&run_id), None, None)[0].stale);

    // The bridge that scanned a repository's output folders is gone from the deck kernel.
    for method in ["analyst.scan", "analyst.import"] {
        let err = deck
            .ws
            .kernel
            .call_value(method, json!({"repo_abs": fx.rt.repo.to_string_lossy()}))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Some(-32601), "{method}: {err}");
    }
    fx.wb.shutdown().await;
}
