//! Integration tests for `KernelClient` against the stdlib-only fake kernel
//! (`tests/fake_kernel.py`), launched through an injected command line the way
//! `ANALYST_TOOLING_PYTHON` would select the interpreter.

use analyst_core::kernel::{KernelArgs, KernelClient, LocalStdio};
use analyst_protocol::kernel::*;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fake_kernel() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_kernel.py")
}

fn launcher() -> Vec<String> {
    // Mirrors `ANALYST_TOOLING_PYTHON`-style injection: the first element is the interpreter.
    let py = std::env::var("ANALYST_TOOLING_PYTHON").unwrap_or_else(|_| "python3".to_string());
    vec![py, fake_kernel().to_string_lossy().to_string()]
}

struct Fixture {
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    transport: LocalStdio,
    root: PathBuf,
}

fn fixture(script: &str) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("scripts")).unwrap();
    std::fs::write(repo.path().join("scripts/hello.py"), script).unwrap();
    let runs = home.path().join("runs");
    let args = KernelArgs {
        python: "python3".into(),
        root: repo.path().to_string_lossy().to_string(),
        runs_dir: runs.to_string_lossy().to_string(),
        out_dir: None,
        sandbox: Some("none".into()),
        data_dirs: vec![],
    };
    Fixture {
        root: repo.path().to_path_buf(),
        transport: LocalStdio::with_launcher(launcher(), args),
        _home: home,
        _repo: repo,
    }
}

const FAST_SCRIPT: &str = r#"
import os, sys
print("hello from the script")
print("second line", flush=True)
out = os.environ["ANALYST_OUT_DIR"]
os.makedirs(os.path.join(out, "figures"), exist_ok=True)
with open(os.path.join(out, "figures", "fig-1.png"), "wb") as f:
    f.write(b"\x89PNG not really")
print("args:", sys.argv[1:])
"#;

const SLOW_SCRIPT: &str = r#"
import time
for i in range(6):
    print("tick", i, flush=True)
    time.sleep(0.4)
"#;

#[tokio::test]
async fn hello_info_index_run_and_shutdown() {
    let fx = fixture(FAST_SCRIPT);
    let client = KernelClient::connect(Box::new(fx.transport.clone()), "test")
        .await
        .expect("connect");
    let hello = client.hello().await.unwrap();
    assert_eq!(hello.protocol_version, 1);
    assert_eq!(hello.root, fx.root.to_string_lossy());
    let info = client.env_info().await.unwrap();
    assert_eq!(info.python, "python3");
    let idx = client.scripts_index(None).await.unwrap();
    assert_eq!(idx.scripts[0].name, "fit_decay");
    assert_eq!(idx.scripts[0].options[1].dest, "seed");
    let lib = client.lib_index(None).await.unwrap();
    assert!(lib.functions[0].has_math);

    // Unknown method -> RPC error surfaces as KernelError::Rpc.
    let err = client
        .call_value("nope.method", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("-32601"));

    let mut rx = client.subscribe();
    let started = client
        .run_start(RunStartParams {
            script: "scripts/hello.py".into(),
            args: Some(vec!["--x".into(), "1".into()]),
            timeout_s: Some(30.0),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(started.out_dir.starts_with("out/hello/"));

    let (done, stdout, _stderr, artifacts) =
        analyst_core::evals::wait_for_run(&mut rx, &started.run_id, Duration::from_secs(20)).await;
    let done = done.expect("run.done");
    assert_eq!(done.status, RunStatus::Done);
    assert_eq!(done.exit_code, Some(0));
    assert!(stdout.contains("hello from the script"));
    assert!(stdout.contains("args: ['--x', '1']"));
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].kind, ArtifactKind::Figure);
    assert!(artifacts[0].rendered);
    assert!(fx.root.join(&artifacts[0].path).is_file());
    assert!(client.active_runs().await.is_empty());

    let meta = client.run_status(&started.run_id).await.unwrap();
    assert_eq!(meta.status, RunStatus::Done);
    let list = client.runs_list(Some(5)).await.unwrap();
    assert_eq!(list.runs[0].run_id, started.run_id);
    let not_found = client.run_status("missing").await.unwrap_err();
    assert!(not_found.to_string().contains("1000"));

    let ex = client.exec_snippet("print(6*7)", None).await.unwrap();
    assert!(ex.ok);
    assert_eq!(ex.stdout.trim(), "42");

    client.shutdown().await.unwrap();
    assert!(!client.is_connected().await);
}

#[tokio::test]
async fn reconnect_reattaches_running_runs() {
    let fx = fixture(SLOW_SCRIPT);
    let client = KernelClient::connect(Box::new(fx.transport.clone()), "test")
        .await
        .expect("connect");
    let mut rx = client.subscribe();
    let started = client
        .run_start(RunStartParams {
            script: "scripts/hello.py".into(),
            timeout_s: Some(30.0),
            ..Default::default()
        })
        .await
        .unwrap();
    // Wait for the first stdout chunk, then drop the kernel and respawn.
    let mut before = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Ok(KernelNotification::Stdout(o))) if o.run_id == started.run_id => {
                before.push_str(&o.chunk);
                break;
            }
            Ok(Ok(_)) => continue,
            _ => panic!("no stdout before reconnect"),
        }
    }
    let offsets_before = client.active_runs().await;
    assert_eq!(offsets_before.len(), 1);
    assert!(offsets_before[0].1.stdout > 0);

    let hello = client.reconnect("test").await.expect("reconnect");
    assert_eq!(hello.protocol_version, 1);
    assert!(client.is_connected().await);

    // After reconnect, run.attach replays from the recorded offsets: the remaining ticks and
    // run.done arrive on the same subscription.
    let (done, after, _, _) =
        analyst_core::evals::wait_for_run(&mut rx, &started.run_id, Duration::from_secs(20)).await;
    let done = done.expect("run.done after reconnect");
    assert_eq!(done.status, RunStatus::Done);
    let all = format!("{before}{after}");
    for i in 0..6 {
        assert!(
            all.contains(&format!("tick {i}")),
            "missing tick {i} in {all:?}"
        );
    }
    assert_eq!(all.matches("tick 0").count(), 1, "no duplicate replay");
    assert!(client.active_runs().await.is_empty());
    client.shutdown().await.unwrap();
}

/// R6: a kernel that stops answering must fail the call instead of hanging forever.
#[tokio::test]
async fn calls_time_out_when_the_kernel_stops_answering() {
    let fx = fixture(FAST_SCRIPT);
    let client = KernelClient::connect(Box::new(fx.transport.clone()), "test")
        .await
        .expect("connect");
    assert_eq!(client.call_timeout(), Duration::from_secs(120));
    // The snippet outlives the limit given to this one call.
    let started = std::time::Instant::now();
    let err = client
        .call_value_with_timeout(
            "exec.snippet",
            serde_json::json!({"code": "import time; time.sleep(5)"}),
            Duration::from_millis(300),
        )
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(4));
    let msg = err.to_string();
    assert!(
        msg.contains("exec.snippet") && msg.contains("did not answer"),
        "{msg}"
    );
    assert!(matches!(
        err,
        analyst_core::kernel::KernelError::Timeout { .. }
    ));
    // The default limit is configurable.
    client.set_call_timeout(Duration::from_millis(200));
    assert_eq!(client.call_timeout(), Duration::from_millis(200));
}

#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    // A killed but not yet reaped child is a zombie: its status line says so.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat.contains(") Z "),
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
async fn wait_gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// R8: abandoning a connect (timeout) must not leave the spawned process behind.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn abandoned_connect_kills_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    // A "kernel" that never answers hello (like ssh waiting on a dead host).
    let code = format!(
        "import os,time; open({:?},'w').write(str(os.getpid())); time.sleep(600)",
        pidfile.to_string_lossy()
    );
    let args = KernelArgs {
        python: "python3".into(),
        root: dir.path().to_string_lossy().to_string(),
        runs_dir: dir.path().to_string_lossy().to_string(),
        out_dir: None,
        sandbox: None,
        data_dirs: vec![],
    };
    let transport = LocalStdio::with_launcher(vec!["python3".into(), "-c".into(), code], args);
    let r = tokio::time::timeout(
        Duration::from_secs(3),
        KernelClient::connect(Box::new(transport), "test"),
    )
    .await;
    assert!(r.is_err(), "connect should still be waiting for hello");
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_gone(pid).await,
        "process {pid} leaked after the abandoned connect"
    );
}

/// R8: dropping the last client handle ends the kernel process.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_the_last_handle_kills_the_kernel() {
    let fx = fixture(FAST_SCRIPT);
    let client = KernelClient::connect(Box::new(fx.transport.clone()), "test")
        .await
        .expect("connect");
    let hello = client.hello().await.unwrap();
    let Some(pid) = kernel_pid(&client).await else {
        eprintln!("skipping: cannot determine the kernel pid ({hello:?})");
        return;
    };
    assert!(alive(pid));
    let second = client.clone();
    drop(client);
    assert!(
        second.is_connected().await,
        "a remaining handle keeps it alive"
    );
    assert!(alive(pid));
    drop(second);
    assert!(
        wait_gone(pid).await,
        "kernel {pid} survived its last handle"
    );
}

/// The kernel's pid, found through a snippet (the fake kernel runs it in-process or in a
/// direct child).
#[cfg(target_os = "linux")]
async fn kernel_pid(client: &KernelClient) -> Option<u32> {
    let r = client
        .exec_snippet("import os; print(os.getpid(), os.getppid())", None)
        .await
        .ok()?;
    let mut it = r.stdout.split_whitespace();
    let me: u32 = it.next()?.parse().ok()?;
    let parent: u32 = it.next()?.parse().ok()?;
    // Whichever of the two is running fake_kernel.py is the kernel.
    for pid in [me, parent] {
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if String::from_utf8_lossy(&cmd).contains("fake_kernel.py") {
            return Some(pid);
        }
    }
    None
}

/// A10: a run whose owner is dropped (turn cancelled) is cancelled on the target.
#[tokio::test]
async fn cancel_guard_cancels_the_run_when_dropped() {
    let fx = fixture(
        "import time\nfor i in range(100):\n    print('tick', i, flush=True)\n    time.sleep(0.2)\n",
    );
    let client = KernelClient::connect(Box::new(fx.transport.clone()), "test")
        .await
        .expect("connect");
    let mut rx = client.subscribe();
    let params = || RunStartParams {
        script: "scripts/hello.py".into(),
        timeout_s: Some(60.0),
        ..Default::default()
    };
    let started = client.run_start(params()).await.unwrap();
    let guard = client.cancel_guard(&started.run_id);
    assert_eq!(guard.run_id(), started.run_id);
    drop(guard);
    let (done, _, _, _) =
        analyst_core::evals::wait_for_run(&mut rx, &started.run_id, Duration::from_secs(15)).await;
    assert_eq!(done.expect("run.done").status, RunStatus::Cancelled);

    // A disarmed guard leaves the run alone.
    let second = client.run_start(params()).await.unwrap();
    client.cancel_guard(&second.run_id).disarm();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        client.run_status(&second.run_id).await.unwrap().status,
        RunStatus::Running
    );
    client.run_cancel(&second.run_id).await.unwrap();
    client.shutdown().await.unwrap();
}

/// A10 through the tool layer: dropping a `run.script` call (what cancelling a turn does)
/// stops the script instead of leaving it running on the target.
#[tokio::test]
async fn dropping_a_run_script_call_cancels_the_run() {
    use analyst_core::session::Session;
    use analyst_core::tools::ToolContext;
    use analyst_protocol::config::ProjectConfig;

    let fx = fixture(
        "import time\nfor i in range(200):\n    print('tick', i, flush=True)\n    time.sleep(0.2)\n",
    );
    let home = tempfile::tempdir().unwrap();
    analyst_core::checkpoint::git(&fx.root, &["init", "-q"], None).unwrap();
    let client = KernelClient::connect(Box::new(fx.transport.clone()), "test")
        .await
        .expect("connect");
    let session = Session::create(home.path(), &fx.root, "local", "p/m").unwrap();
    let mut ctx = ToolContext::new(
        &fx.root,
        home.path(),
        session,
        "c1",
        ProjectConfig::default(),
    );
    ctx.kernel = Some(client.clone());
    let mut rx = client.subscribe();
    let args = serde_json::json!({"script": "scripts/hello.py"});
    // The call is still streaming when its future is dropped.
    let dropped = tokio::time::timeout(
        Duration::from_secs(2),
        ctx.dispatch("t1", "run.script", &args),
    )
    .await;
    assert!(dropped.is_err(), "the run should still be in progress");
    let run_id = client
        .runs_list(Some(1))
        .await
        .unwrap()
        .runs
        .first()
        .map(|r| r.run_id.clone())
        .expect("a run was started");
    let (done, _, _, _) =
        analyst_core::evals::wait_for_run(&mut rx, &run_id, Duration::from_secs(15)).await;
    let status = match done {
        Some(d) => d.status,
        None => client.run_status(&run_id).await.unwrap().status,
    };
    assert_eq!(status, RunStatus::Cancelled);
    client.shutdown().await.unwrap();
}
