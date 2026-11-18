//! Evaluation harness (PROTOCOL 8 and 9): task loading, fixture preparation, running the agent
//! with `AutoApprover`, check executors, and scorecards.
//!
//! The graders only credit what the agent did: runs and artifacts are counted from the runs
//! the agent started during the task (never the grader's own `run_succeeds` runs), `no_edits`
//! compares the working tree with the commit made just before the agent started, and scope
//! is judged per edit against the scope in force when that edit was decided. Each task runs
//! with its own temporary analyst home.

use crate::agent::{Agent, AgentConfig, AutoApprover};
use crate::kernel::{KernelArgs, KernelClient, LocalStdio};
use crate::session::Session;
use crate::tools::gate::Policy;
use crate::tools::ToolContext;
use analyst_protocol::config::{EditPolicy, ProjectConfig, RunPolicy};
use analyst_protocol::evals::*;
use analyst_protocol::kernel::*;
use analyst_protocol::transcript::{Event, ProposalKind};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use wb_providers::model::ModelCapabilities;
use wb_providers::ModelClient;

#[derive(Debug, Clone)]
pub struct LoadedTask {
    pub dir: PathBuf,
    pub file: TaskFile,
}

/// Load `<tasks_dir>/*/task.toml`, optionally only the task named `filter`.
pub fn load_tasks(tasks_dir: &Path, filter: Option<&str>) -> Result<Vec<LoadedTask>> {
    let mut out = Vec::new();
    let rd = std::fs::read_dir(tasks_dir)
        .with_context(|| format!("reading tasks dir {}", tasks_dir.display()))?;
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let path = dir.join("task.toml");
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let file: TaskFile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if let Some(f) = filter {
            if file.task.name != f && dir.file_name().map(|n| n != f).unwrap_or(true) {
                continue;
            }
        }
        out.push(LoadedTask { dir, file });
    }
    if let Some(f) = filter {
        if out.is_empty() {
            return Err(anyhow!("no task named `{f}` under {}", tasks_dir.display()));
        }
    }
    Ok(out)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    for e in walkdir::WalkDir::new(src)
        .min_depth(1)
        .into_iter()
        .flatten()
    {
        let rel = e.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(p) = target.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

/// Copy the fixture into `dest`, `git init` + initial commit, and make sure
/// `.analyst/project.toml` exists.
pub fn prepare_fixture(task: &LoadedTask, dest: &Path) -> Result<()> {
    let src = task.dir.join(&task.file.fixture.dir);
    std::fs::create_dir_all(dest)?;
    if src.is_dir() {
        copy_tree(&src, dest)?;
    }
    let pc = dest.join(".analyst/project.toml");
    if !pc.is_file() {
        std::fs::create_dir_all(dest.join(".analyst"))?;
        std::fs::write(&pc, toml::to_string_pretty(&ProjectConfig::default())?)?;
    }
    let gi = dest.join(".gitignore");
    if !gi.is_file() {
        std::fs::write(&gi, "out/\n.venv/\n__pycache__/\n.analyst/env.sh\n")?;
    }
    crate::checkpoint::git(dest, &["init", "-q"], None)?;
    crate::checkpoint::git(dest, &["add", "-A"], None)?;
    crate::checkpoint::git(
        dest,
        &["commit", "-q", "--allow-empty", "-m", "fixture"],
        None,
    )?;
    Ok(())
}

/// How tasks are run.
pub struct EvalEnv {
    /// The caller's analyst home (configuration and capability cache were read from it).
    /// Tasks never write here: each one gets a temporary home for its session and runs.
    pub home: PathBuf,
    pub model: Arc<dyn ModelClient>,
    pub model_name: String,
    pub caps: ModelCapabilities,
    /// Kernel launcher (`[tooling_python, "-m", "analyst_kernel"]`); `None` runs without a
    /// kernel (kernel-backed checks then fail with a clear detail).
    pub kernel_launcher: Option<Vec<String>>,
    /// Session interpreter passed to the kernel (`--python`); default `python3`.
    pub session_python: Option<String>,
    /// `--auto`: edits=auto, runs=auto regardless of the fixture's policy.
    pub auto: bool,
}

/// Context for check executors.
pub struct CheckContext<'a> {
    pub repo: &'a Path,
    pub kernel: Option<&'a KernelClient>,
    pub session: &'a Session,
    pub conv_id: &'a str,
    pub baseline_similar: &'a [SimilarPair],
    /// Paths that differ from the commit made just before the agent started, captured right
    /// after its turn (before any grader run).
    pub tree_changes: &'a [String],
}

fn pair_key(p: &SimilarPair) -> (String, String, String, String) {
    (
        p.a.path.clone(),
        p.a.qualname.clone(),
        p.b.path.clone(),
        p.b.qualname.clone(),
    )
}

/// Wait for a run to finish, collecting its notifications. `rx` must have been subscribed
/// before the run started.
pub async fn wait_for_run(
    rx: &mut tokio::sync::broadcast::Receiver<KernelNotification>,
    run_id: &str,
    timeout: Duration,
) -> (Option<RunDone>, String, String, Vec<Artifact>) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut artifacts = Vec::new();
    loop {
        let n = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(n)) => n,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            _ => return (None, stdout, stderr, artifacts),
        };
        if n.run_id() != Some(run_id) {
            continue;
        }
        match n {
            KernelNotification::Stdout(o) => stdout.push_str(&o.chunk),
            KernelNotification::Stderr(o) => stderr.push_str(&o.chunk),
            KernelNotification::Artifact(a) => artifacts.push(a.artifact),
            KernelNotification::Done(d) => return (Some(d), stdout, stderr, artifacts),
            KernelNotification::Other { .. } => {}
        }
    }
}

fn result(kind: &str, passed: bool, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        kind: kind.to_string(),
        passed,
        detail: detail.into(),
    }
}

/// One accepted file edit and whether its path was in the scope in force at that moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgedEdit {
    pub path: String,
    pub in_scope: bool,
    /// Whether any scope had been declared in that turn when the edit was decided.
    pub scope_declared: bool,
}

fn is_declare_scope(name: &str) -> bool {
    name == "declare_scope"
}

/// Accepted file edits from a transcript, each judged against the scope in force when it was
/// decided. Events are replayed in order: the scope resets at every user message, and changes
/// only when a `declare_scope` call succeeded (its result carries the scope from then on).
pub fn judged_edits(events: &[Event]) -> Vec<JudgedEdit> {
    let mut scope: Option<BTreeSet<String>> = None;
    let mut declares: BTreeMap<String, Value> = BTreeMap::new();
    let mut call_paths: BTreeMap<String, String> = BTreeMap::new();
    let mut proposals: BTreeMap<String, ProposalKind> = BTreeMap::new();
    let mut out = Vec::new();
    for e in events {
        match e {
            Event::User { .. } => scope = None,
            Event::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                if is_declare_scope(name) {
                    declares.insert(id.clone(), arguments.clone());
                } else if let Some(p) = arguments.get("path").and_then(|p| p.as_str()) {
                    if let Ok(n) = crate::util::normalize_rel_path(p) {
                        call_paths.insert(id.clone(), n);
                    }
                }
            }
            Event::ToolResult { id, ok, result, .. } => {
                if let (true, Some(args)) = (*ok, declares.get(id)) {
                    // Older transcripts have no `scope` in the result: fall back to the call.
                    let files = result
                        .get("scope")
                        .or_else(|| args.get("files"))
                        .and_then(|f| f.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .filter_map(|s| crate::util::normalize_rel_path(s).ok())
                                .collect::<BTreeSet<_>>()
                        })
                        .unwrap_or_default();
                    scope = Some(files);
                }
            }
            Event::Proposal { id, kind, .. } => {
                proposals.insert(id.clone(), *kind);
            }
            Event::Decision {
                proposal_id,
                decision,
                ..
            } if decision.is_accepted() => {
                let is_file_change = proposals
                    .get(proposal_id)
                    .map(|k| k.is_file_change())
                    .unwrap_or(false);
                if let (true, Some(p)) = (is_file_change, call_paths.get(proposal_id)) {
                    out.push(JudgedEdit {
                        path: p.clone(),
                        in_scope: scope.as_ref().is_some_and(|s| s.contains(p)),
                        scope_declared: scope.is_some(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Accepted edits that were outside the scope in force when they were decided.
pub fn unwanted_edits(events: &[Event]) -> u32 {
    judged_edits(events).iter().filter(|e| !e.in_scope).count() as u32
}

/// Run ids the agent started during the task, with their scripts (from `run` events).
pub fn agent_runs(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Run { run_id, script, .. } => Some((run_id.clone(), script.clone())),
            _ => None,
        })
        .collect()
}

/// Paths that differ from `HEAD` in the working tree (modified, deleted, or untracked and not
/// ignored).
pub fn tree_changes(repo: &Path) -> Result<Vec<String>> {
    let out = crate::checkpoint::git(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        None,
    )?;
    let mut paths = Vec::new();
    let mut it = out.split('\0').filter(|r| !r.is_empty());
    while let Some(rec) = it.next() {
        if rec.len() < 4 {
            continue;
        }
        let (status, path) = rec.split_at(3);
        paths.push(path.to_string());
        if status.starts_with('R') || status.starts_with('C') {
            // Renames carry the original path as the next record.
            if let Some(orig) = it.next() {
                paths.push(orig.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Execute one check.
pub async fn run_check(cx: &CheckContext<'_>, check: &Check) -> CheckResult {
    let kind = check.kind();
    let need_kernel = || result(kind, false, "kernel unavailable");
    match check {
        Check::ContractClean => {
            let Some(k) = cx.kernel else {
                return need_kernel();
            };
            match k.contract_check(None).await {
                Ok(r) => {
                    let errors: Vec<String> = r
                        .diagnostics
                        .iter()
                        .filter(|d| d.severity == Severity::Error)
                        .map(|d| format!("{} {}:{} {}", d.rule, d.path, d.line, d.message))
                        .collect();
                    result(
                        kind,
                        errors.is_empty(),
                        if errors.is_empty() {
                            format!("0 errors, {} warnings", r.diagnostics.len())
                        } else {
                            errors.join("; ")
                        },
                    )
                }
                Err(e) => result(kind, false, format!("contract.check failed: {e}")),
            }
        }
        Check::ScriptExists { path } => {
            let ok = cx.repo.join(path).is_file() && path.ends_with(".py");
            result(
                kind,
                ok,
                if ok {
                    format!("{path} exists")
                } else {
                    format!("{path} missing")
                },
            )
        }
        Check::RunSucceeds { script, args } => {
            let Some(k) = cx.kernel else {
                return need_kernel();
            };
            let mut rx = k.subscribe();
            let started = match k
                .run_start(RunStartParams {
                    script: script.clone(),
                    args: Some(args.clone()),
                    timeout_s: Some(300.0),
                    ..Default::default()
                })
                .await
            {
                Ok(s) => s,
                Err(e) => return result(kind, false, format!("run.start failed: {e}")),
            };
            // The grader's own run is deliberately not recorded in the session: it must not
            // count towards `runs_at_least` or `artifact_exists`.
            let (done, _out, err, _) =
                wait_for_run(&mut rx, &started.run_id, Duration::from_secs(330)).await;
            match done {
                Some(d) if d.status == RunStatus::Done && d.exit_code == Some(0) => result(
                    kind,
                    true,
                    format!("{script} {} exit 0 in {:.1}s", args.join(" "), d.duration_s),
                ),
                Some(d) => result(
                    kind,
                    false,
                    format!(
                        "{script} {}: {} exit {:?}; stderr tail: {}",
                        args.join(" "),
                        d.status,
                        d.exit_code,
                        tail(&err, 600)
                    ),
                ),
                None => result(kind, false, format!("{script}: no run.done received")),
            }
        }
        Check::ArtifactExists { glob, min_count } => {
            // Only files inside the output directories of runs the agent started count.
            let events = cx.session.read_transcript(cx.conv_id).unwrap_or_default();
            let ids: BTreeSet<String> = agent_runs(&events).into_iter().map(|(id, _)| id).collect();
            let run_dirs: Vec<PathBuf> = cx
                .session
                .read_runs()
                .unwrap_or_default()
                .into_iter()
                .filter(|r| ids.contains(&r.meta.run_id) && !r.meta.out_dir.is_empty())
                .map(|r| cx.repo.join(&r.meta.out_dir))
                .collect();
            let pattern = cx.repo.join(glob).to_string_lossy().to_string();
            let count = glob::glob(&pattern)
                .map(|g| {
                    g.flatten()
                        .filter(|p| p.is_file() && run_dirs.iter().any(|d| p.starts_with(d)))
                        .count()
                })
                .unwrap_or(0);
            result(
                kind,
                count as u64 >= *min_count,
                format!(
                    "{count} file(s) from {} agent run(s) match {glob} (need {min_count})",
                    ids.len()
                ),
            )
        }
        Check::EqVerified { path, function } => {
            let Some(k) = cx.kernel else {
                return need_kernel();
            };
            match k
                .eq_check(EqCheckParams {
                    path: path.clone(),
                    function: function.clone(),
                    trials: None,
                    tol: None,
                    ranges: None,
                })
                .await
            {
                Ok(r) => result(
                    kind,
                    r.status == EqCheckStatus::Agree,
                    format!(
                        "{function}: {:?} (trials {}, max_rel_err {:?}) {}",
                        r.status,
                        r.trials,
                        r.max_rel_err,
                        r.reason.unwrap_or_default()
                    ),
                ),
                Err(e) => result(kind, false, format!("eq.check failed: {e}")),
            }
        }
        Check::NoNewDuplicates => {
            let Some(k) = cx.kernel else {
                return need_kernel();
            };
            match k.contract_similar(None).await {
                Ok(r) => {
                    let base: BTreeSet<_> = cx
                        .baseline_similar
                        .iter()
                        .filter(|p| p.kind == SimilarKind::Duplicate)
                        .map(pair_key)
                        .collect();
                    let new: Vec<String> = r
                        .pairs
                        .iter()
                        .filter(|p| {
                            p.kind == SimilarKind::Duplicate && !base.contains(&pair_key(p))
                        })
                        .map(|p| {
                            format!(
                                "{}:{} ~ {}:{}",
                                p.a.path, p.a.qualname, p.b.path, p.b.qualname
                            )
                        })
                        .collect();
                    result(
                        kind,
                        new.is_empty(),
                        if new.is_empty() {
                            "no new duplicate pairs".to_string()
                        } else {
                            new.join("; ")
                        },
                    )
                }
                Err(e) => result(kind, false, format!("contract.similar failed: {e}")),
            }
        }
        Check::EditsWithinScope => {
            let events = cx.session.read_transcript(cx.conv_id).unwrap_or_default();
            let edits = judged_edits(&events);
            if edits.is_empty() {
                return result(kind, true, "no accepted edits");
            }
            let outside: Vec<String> = edits
                .iter()
                .filter(|e| !e.in_scope)
                .map(|e| {
                    if e.scope_declared {
                        e.path.clone()
                    } else {
                        format!("{} (no scope declared yet)", e.path)
                    }
                })
                .collect();
            result(
                kind,
                outside.is_empty(),
                if outside.is_empty() {
                    format!("{} edit(s) all in scope", edits.len())
                } else {
                    format!("outside scope when decided: {}", outside.join(", "))
                },
            )
        }
        Check::FileMatches { path, regex } => {
            let re = match regex::Regex::new(regex) {
                Ok(r) => r,
                Err(e) => return result(kind, false, format!("bad regex: {e}")),
            };
            match std::fs::read_to_string(cx.repo.join(path)) {
                Ok(t) => {
                    let ok = re.is_match(&t);
                    result(
                        kind,
                        ok,
                        if ok {
                            format!("{path} matches /{regex}/")
                        } else {
                            format!("{path} does not match /{regex}/")
                        },
                    )
                }
                Err(e) => result(kind, false, format!("{path}: {e}")),
            }
        }
        Check::NoEdits => {
            // The tree itself is compared, so writes that never went through a proposal
            // (a snippet, a script) are seen too.
            result(
                kind,
                cx.tree_changes.is_empty(),
                if cx.tree_changes.is_empty() {
                    "working tree identical to the pre-task commit".to_string()
                } else {
                    format!("changed: {}", cx.tree_changes.join(", "))
                },
            )
        }
        Check::RunsAtLeast { script, count } => {
            let events = cx.session.read_transcript(cx.conv_id).unwrap_or_default();
            let ids: BTreeSet<String> = agent_runs(&events)
                .into_iter()
                .filter(|(_, s)| s == script)
                .map(|(id, _)| id)
                .collect();
            result(
                kind,
                ids.len() as u64 >= *count,
                format!("{} agent run(s) of {script} (need {count})", ids.len()),
            )
        }
    }
}

fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut start = s.len() - n;
        while !s.is_char_boundary(start) {
            start += 1;
        }
        format!("...{}", &s[start..])
    }
}

/// Run one task end to end.
pub async fn run_task(env: &EvalEnv, task: &LoadedTask) -> Result<TaskResult> {
    let name = task.file.task.name.clone();
    let work = tempfile::Builder::new()
        .prefix(&format!("analyst-eval-{name}-"))
        .tempdir()?;
    let repo = work.path().join("repo");
    prepare_fixture(task, &repo)?;
    let repo = std::fs::canonicalize(&repo)?;
    let session_python = match &env.session_python {
        Some(p) => p.clone(),
        None => fixture_python(&repo).await,
    };
    // Everything the task writes outside the repo goes to a home of its own, removed with
    // the task: sessions, transcripts, run records.
    let task_home = work.path().join("home");
    std::fs::create_dir_all(&task_home)?;
    // Commit the prepared environment (for example a `uv.lock` written by `uv sync`), so the
    // tree diff after the turn shows only what happened during the task.
    crate::checkpoint::git(&repo, &["add", "-A"], None)?;
    crate::checkpoint::git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "pre-task baseline"],
        None,
    )?;
    let mut project = crate::config::load_project(&repo)?;
    if env.auto {
        project.policy.edits = EditPolicy::Auto;
        project.policy.runs = RunPolicy::Auto;
    }
    project.policy.max_steps = task.file.task.max_steps;

    let kernel = match &env.kernel_launcher {
        Some(launcher) => {
            let runs_dir = task_home.join("runs");
            std::fs::create_dir_all(&runs_dir)?;
            let args = KernelArgs {
                python: session_python.clone(),
                root: repo.to_string_lossy().to_string(),
                runs_dir: runs_dir.to_string_lossy().to_string(),
                out_dir: Some(project.project.out_dir.clone()),
                sandbox: Some("auto".into()),
                data_dirs: vec![],
            };
            match KernelClient::connect(
                Box::new(LocalStdio::with_launcher(launcher.clone(), args)),
                crate::CORE_VERSION,
            )
            .await
            {
                Ok(k) => Some(k),
                Err(e) => {
                    tracing::warn!("kernel connect failed for task {name}: {e}");
                    None
                }
            }
        }
        None => None,
    };
    let baseline = match &kernel {
        Some(k) => k
            .contract_similar(None)
            .await
            .map(|r| r.pairs)
            .unwrap_or_default(),
        None => vec![],
    };

    let session = Session::create(&task_home, &repo, "local", &env.model_name)?;
    let conv_id = "eval".to_string();
    let mut ctx = ToolContext::new(&repo, &task_home, session, &conv_id, project.clone());
    ctx.kernel = kernel.clone();
    let policy = Policy::for_project(&repo, &project, task.file.task.mode)?;
    let config = AgentConfig::new(
        task.file.task.mode,
        &env.model_name,
        env.caps.clone(),
        task.file.task.max_steps,
    );
    let mut agent = Agent::new(
        env.model.clone(),
        ctx,
        policy,
        Arc::new(AutoApprover),
        config,
    );

    let start = std::time::Instant::now();
    let turn = tokio::time::timeout(
        Duration::from_secs(task.file.task.timeout_s),
        agent.run_turn(&task.file.task.prompt),
    )
    .await;
    let wall_s = start.elapsed().as_secs_f64();
    let (summary, turn_error) = match turn {
        Ok(Ok(s)) => (s, None),
        Ok(Err(e)) => (Default::default(), Some(format!("turn failed: {e}"))),
        Err(_) => (
            Default::default(),
            Some(format!(
                "task timed out after {}s",
                task.file.task.timeout_s
            )),
        ),
    };

    // Captured before any grader run can touch the tree.
    let changes = tree_changes(&repo).unwrap_or_else(|e| vec![format!("(git status failed: {e})")]);
    let events = agent
        .ctx
        .session
        .read_transcript(&conv_id)
        .unwrap_or_default();
    let cx = CheckContext {
        repo: &repo,
        kernel: kernel.as_ref(),
        session: &agent.ctx.session,
        conv_id: &conv_id,
        baseline_similar: &baseline,
        tree_changes: &changes,
    };
    let mut checks = Vec::new();
    for c in &task.file.checks {
        checks.push(run_check(&cx, c).await);
    }
    if let Some(e) = turn_error {
        checks.push(CheckResult {
            kind: "agent_turn".into(),
            passed: false,
            detail: e,
        });
    }
    if let Some(k) = &kernel {
        let _ = k.shutdown().await;
    }
    let passed = checks.iter().all(|c| c.passed);
    Ok(TaskResult {
        name,
        passed,
        checks,
        steps: summary.steps,
        input_tokens: summary.input_tokens,
        output_tokens: summary.output_tokens,
        wall_s,
        // From the transcript, so it is right even when the turn failed or timed out.
        unwanted_edits: unwanted_edits(&events),
        tool_errors: summary.tool_errors,
        tool_calls: summary.tool_calls,
    })
}

/// Session interpreter for a fixture: `uv sync` when a `pyproject.toml` exists and `uv` is on
/// PATH, then `.venv/bin/python`; else `python3`.
async fn fixture_python(repo: &Path) -> String {
    if repo.join("pyproject.toml").is_file() && crate::util::find_tool("uv").is_some() {
        match crate::target::run_command(
            &crate::egress::uv_install("uv", &["sync"], &["-q"]),
            Some(repo),
        )
        .await
        {
            Ok(_) => {}
            Err(e) => tracing::warn!("uv sync in fixture failed: {e}"),
        }
    }
    let venv = repo.join(".venv/bin/python");
    if venv.is_file() {
        venv.to_string_lossy().to_string()
    } else {
        crate::util::tool("python3")
    }
}

fn rate(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

/// Build the scorecard summary.
pub fn scorecard(model: &str, tasks: Vec<TaskResult>) -> Scorecard {
    let n = tasks.len();
    let passed = tasks.iter().filter(|t| t.passed).count();
    let with = |kind: &str| -> (usize, usize) {
        let have: Vec<&TaskResult> = tasks
            .iter()
            .filter(|t| t.checks.iter().any(|c| c.kind == kind))
            .collect();
        let ok = have
            .iter()
            .filter(|t| t.checks.iter().filter(|c| c.kind == kind).all(|c| c.passed))
            .count();
        (ok, have.len())
    };
    let (c_ok, c_n) = with("contract_clean");
    let (e_ok, e_n) = with("eq_verified");
    let unwanted = tasks.iter().filter(|t| t.unwanted_edits > 0).count();
    let calls: u32 = tasks.iter().map(|t| t.tool_calls).sum();
    let errors: u32 = tasks.iter().map(|t| t.tool_errors).sum();
    Scorecard {
        model: model.to_string(),
        tasks,
        summary: ScorecardSummary {
            pass_rate: rate(passed, n),
            contract_rate: rate(c_ok, c_n),
            eq_rate: rate(e_ok, e_n),
            unwanted_edit_rate: rate(unwanted, n),
            tool_error_rate: rate(errors as usize, calls as usize),
        },
    }
}

/// Markdown table for stdout.
pub fn markdown_table(sc: &Scorecard) -> String {
    let mut s = String::new();
    s.push_str("| task | passed | checks | steps | tokens in/out | wall s | unwanted edits | tool errors |\n");
    s.push_str("|---|---|---|---|---|---|---|---|\n");
    for t in &sc.tasks {
        let ok = t.checks.iter().filter(|c| c.passed).count();
        s.push_str(&format!(
            "| {} | {} | {}/{} | {} | {}/{} | {:.1} | {} | {}/{} |\n",
            t.name,
            if t.passed { "yes" } else { "no" },
            ok,
            t.checks.len(),
            t.steps,
            t.input_tokens,
            t.output_tokens,
            t.wall_s,
            t.unwanted_edits,
            t.tool_errors,
            t.tool_calls
        ));
    }
    let m = &sc.summary;
    s.push_str(&format!(
        "\npass {:.0}% | contract {:.0}% | eq {:.0}% | unwanted-edit {:.0}% | tool-error {:.1}%\n",
        m.pass_rate * 100.0,
        m.contract_rate * 100.0,
        m.eq_rate * 100.0,
        m.unwanted_edit_rate * 100.0,
        m.tool_error_rate * 100.0
    ));
    for t in &sc.tasks {
        for c in t.checks.iter().filter(|c| !c.passed) {
            s.push_str(&format!("- {} / {}: {}\n", t.name, c.kind, c.detail));
        }
    }
    s
}

/// Write `evals/results/<model>-<timestamp>.json`; returns the path.
pub fn write_scorecard(results_dir: &Path, sc: &Scorecard) -> Result<PathBuf> {
    std::fs::create_dir_all(results_dir)?;
    let safe: String = sc
        .model
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = results_dir.join(format!("{safe}-{}.json", crate::util::now_stamp()));
    std::fs::write(&path, serde_json::to_string_pretty(sc)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wb_providers::model::ModelEvent;
    use wb_providers::ScriptedModelClient;

    fn fake_kernel_launcher() -> Vec<String> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_kernel.py");
        vec!["python3".into(), p.to_string_lossy().to_string()]
    }

    fn write_task(dir: &Path) {
        let fixture = dir.join("fixture");
        std::fs::create_dir_all(fixture.join("scripts")).unwrap();
        std::fs::create_dir_all(fixture.join("analysis")).unwrap();
        std::fs::write(fixture.join("analysis/__init__.py"), "").unwrap();
        std::fs::write(fixture.join("analysis/fitting.py"), "def double_exp(t, A, tau1, B, tau2):\n    \"\"\"Math:\n        y = A*exp(-t/tau1) + B*exp(-t/tau2)\n    \"\"\"\n    return A\n").unwrap();
        std::fs::write(
            fixture.join("scripts/fit_decay.py"),
            "import argparse, os, time\n\ndef main():\n    p = argparse.ArgumentParser(description='fit')\n    p.add_argument('--n', type=int, default=2, help='count')\n    a = p.parse_args()\n    out = os.environ.get('ANALYST_OUT_DIR', 'out')\n    os.makedirs(os.path.join(out, 'figures'), exist_ok=True)\n    for i in range(a.n):\n        with open(os.path.join(out, 'figures', f'fig-{i+1}.png'), 'wb') as f:\n            f.write(b'\\x89PNG fake')\n        print('line', i)\n\nif __name__ == '__main__':\n    main()\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("task.toml"),
            r#"
[task]
name = "smoke"
prompt = "Do nothing."
mode = "build"
timeout_s = 120
max_steps = 5

[fixture]
dir = "fixture"

[[checks]]
kind = "contract_clean"
[[checks]]
kind = "script_exists"
path = "scripts/fit_decay.py"
[[checks]]
kind = "run_succeeds"
script = "scripts/fit_decay.py"
args = ["--n", "2"]
[[checks]]
kind = "artifact_exists"
glob = "out/fit_decay/*/figures/*.png"
min_count = 2
[[checks]]
kind = "eq_verified"
path = "analysis/fitting.py"
function = "double_exp"
[[checks]]
kind = "no_new_duplicates"
[[checks]]
kind = "edits_within_scope"
[[checks]]
kind = "file_matches"
path = "analysis/fitting.py"
regex = "def double_exp\\("
[[checks]]
kind = "no_edits"
[[checks]]
kind = "runs_at_least"
script = "scripts/fit_decay.py"
count = 1
"#,
        )
        .unwrap();
    }

    fn env_with(home: &Path, scripts: Vec<Vec<ModelEvent>>) -> EvalEnv {
        EvalEnv {
            home: home.to_path_buf(),
            model: Arc::new(ScriptedModelClient::new(scripts)),
            model_name: "scripted/model".into(),
            caps: ModelCapabilities::default(),
            kernel_launcher: Some(fake_kernel_launcher()),
            session_python: Some("python3".into()),
            auto: true,
        }
    }

    fn tool(id: &str, name: &str, args: serde_json::Value) -> ModelEvent {
        ModelEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }
    }

    fn stop_tools() -> ModelEvent {
        ModelEvent::Stop {
            reason: wb_providers::model::StopReason::ToolCalls,
            message: None,
        }
    }

    fn check<'a>(r: &'a TaskResult, kind: &str) -> &'a CheckResult {
        r.checks.iter().find(|c| c.kind == kind).unwrap()
    }

    /// "Tests that pass for the wrong reason": an agent that does nothing must not be
    /// credited with the grader's own run or the artifacts that run produced.
    #[tokio::test]
    async fn idle_agent_gets_no_credit_for_runs_or_artifacts() {
        let tasks = tempfile::tempdir().unwrap();
        let tdir = tasks.path().join("smoke");
        std::fs::create_dir_all(&tdir).unwrap();
        write_task(&tdir);
        let loaded = load_tasks(tasks.path(), Some("smoke")).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].file.checks.len(), 10);
        let home = tempfile::tempdir().unwrap();
        let env = env_with(
            home.path(),
            vec![vec![ModelEvent::Text {
                delta: "Nothing to do.".into(),
            }]],
        );
        let r = run_task(&env, &loaded[0]).await.unwrap();
        // The grader's own run still succeeds...
        assert!(
            check(&r, "run_succeeds").passed,
            "{}",
            check(&r, "run_succeeds").detail
        );
        // ...but it earns the agent nothing.
        for kind in ["runs_at_least", "artifact_exists"] {
            let c = check(&r, kind);
            assert!(!c.passed, "{kind} passed for an idle agent: {}", c.detail);
        }
        assert!(check(&r, "runs_at_least")
            .detail
            .starts_with("0 agent run(s)"));
        assert!(check(&r, "artifact_exists").detail.starts_with("0 file(s)"));
        for kind in [
            "contract_clean",
            "script_exists",
            "eq_verified",
            "no_new_duplicates",
            "edits_within_scope",
            "file_matches",
            "no_edits",
        ] {
            let c = check(&r, kind);
            assert!(c.passed, "{kind} failed: {}", c.detail);
        }
        assert!(!r.passed);
        assert_eq!(r.steps, 1);
        assert_eq!(r.unwanted_edits, 0);
        // Nothing was written into the caller's analyst home.
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        let sc = scorecard("scripted/model", vec![r]);
        assert_eq!(sc.summary.pass_rate, 0.0);
        let md = markdown_table(&sc);
        assert!(md.contains("| smoke | no | 8/10 |"), "{md}");
        let results = tempfile::tempdir().unwrap();
        let p = write_scorecard(results.path(), &sc).unwrap();
        let back: Scorecard = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(back, sc);
    }

    /// The same task passes when the agent actually runs the script.
    #[tokio::test]
    async fn agent_runs_earn_run_and_artifact_credit() {
        let tasks = tempfile::tempdir().unwrap();
        let tdir = tasks.path().join("smoke");
        std::fs::create_dir_all(&tdir).unwrap();
        write_task(&tdir);
        let loaded = load_tasks(tasks.path(), Some("smoke")).unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = env_with(
            home.path(),
            vec![
                vec![
                    tool(
                        "r1",
                        "run_script",
                        serde_json::json!({"script": "scripts/fit_decay.py", "args": ["--n", "2"]}),
                    ),
                    stop_tools(),
                ],
                vec![ModelEvent::Text {
                    delta: "Ran it.".into(),
                }],
            ],
        );
        let r = run_task(&env, &loaded[0]).await.unwrap();
        for c in &r.checks {
            assert!(c.passed, "{} failed: {}", c.kind, c.detail);
        }
        assert!(r.passed);
        assert!(check(&r, "runs_at_least")
            .detail
            .starts_with("1 agent run(s)"));
        assert!(check(&r, "artifact_exists").detail.starts_with("2 file(s)"));
    }

    /// `no_edits` looks at the tree: a file written outside the proposal path is an edit.
    #[tokio::test]
    async fn no_edits_sees_changes_that_bypass_proposals() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        crate::checkpoint::git(r, &["init", "-q"], None).unwrap();
        std::fs::write(r.join(".gitignore"), "out/\n").unwrap();
        std::fs::write(r.join("a.py"), "a = 1\n").unwrap();
        std::fs::write(r.join("b.py"), "b = 1\n").unwrap();
        crate::checkpoint::git(r, &["add", "-A"], None).unwrap();
        crate::checkpoint::git(r, &["commit", "-q", "-m", "base"], None).unwrap();
        assert!(tree_changes(r).unwrap().is_empty());
        // What a snippet would do: no proposal, no decision event.
        std::fs::write(r.join("a.py"), "a = 2\n").unwrap();
        std::fs::write(r.join("new file.py"), "n\n").unwrap();
        std::fs::remove_file(r.join("b.py")).unwrap();
        std::fs::create_dir_all(r.join("out")).unwrap();
        std::fs::write(r.join("out/ignored.txt"), "x").unwrap();
        let changes = tree_changes(r).unwrap();
        assert_eq!(changes, vec!["a.py", "b.py", "new file.py"]);
        let home = tempfile::tempdir().unwrap();
        let session = Session::create(home.path(), r, "local", "p/m").unwrap();
        let cx = CheckContext {
            repo: r,
            kernel: None,
            session: &session,
            conv_id: "c",
            baseline_similar: &[],
            tree_changes: &changes,
        };
        let res = run_check(&cx, &Check::NoEdits).await;
        assert!(!res.passed);
        assert!(res.detail.contains("a.py"));
    }

    #[test]
    fn edits_are_judged_against_the_scope_in_force() {
        use analyst_protocol::transcript::{DecidedBy, Decision};
        let user = || Event::User {
            ts: "t".into(),
            text: "go".into(),
            attachments: None,
        };
        let declare = |id: &str, files: &[&str], ok: bool| {
            vec![
                Event::ToolCall {
                    ts: "t".into(),
                    id: id.into(),
                    name: "declare_scope".into(),
                    arguments: serde_json::json!({"files": files}),
                },
                Event::ToolResult {
                    ts: "t".into(),
                    id: id.into(),
                    ok,
                    result: if ok {
                        serde_json::json!({"ok": true, "scope": files})
                    } else {
                        serde_json::json!({"error": "not approved"})
                    },
                    truncated: None,
                },
            ]
        };
        let edit = |id: &str, path: &str, decision: Decision, by: DecidedBy| {
            vec![
                Event::ToolCall {
                    ts: "t".into(),
                    id: id.into(),
                    name: "fs_propose_edit".into(),
                    arguments: serde_json::json!({"path": path}),
                },
                Event::Proposal {
                    ts: "t".into(),
                    id: id.into(),
                    kind: ProposalKind::Edit,
                    summary: "".into(),
                    diff: None,
                },
                Event::Decision {
                    ts: "t".into(),
                    proposal_id: id.into(),
                    decision,
                    by,
                },
            ]
        };
        let mut ev = vec![user()];
        // Edit first, declare afterwards: the old grader called this in scope.
        ev.extend(edit("e0", "early.py", Decision::Accept, DecidedBy::User));
        ev.extend(declare("s1", &["early.py", "a.py"], true));
        ev.extend(edit("e1", "a.py", Decision::Auto, DecidedBy::Policy));
        ev.extend(edit("e2", "b.py", Decision::Reject, DecidedBy::User));
        // A rejected widening changes nothing.
        ev.extend(declare("s2", &["a.py", "c.py"], false));
        ev.extend(edit("e3", "c.py", Decision::Accept, DecidedBy::User));
        // Next turn: the scope is gone.
        ev.push(user());
        ev.extend(edit("e4", "a.py", Decision::Accept, DecidedBy::User));
        let judged = judged_edits(&ev);
        let got: Vec<(&str, bool, bool)> = judged
            .iter()
            .map(|e| (e.path.as_str(), e.in_scope, e.scope_declared))
            .collect();
        assert_eq!(
            got,
            vec![
                ("early.py", false, false),
                ("a.py", true, true),
                ("c.py", false, true),
                ("a.py", false, false),
            ]
        );
        // Only accepted out-of-scope edits are unwanted; the in-scope accept is not.
        assert_eq!(unwanted_edits(&ev), 3);
        assert_eq!(unwanted_edits(&ev[..10]), 1);
    }
}
