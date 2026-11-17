//! Tool execution. Every result is capped at 16 KiB for the model; larger results are stored
//! under the session's `artifacts/` and the model receives the head plus
//! `{truncated: true, artifact: "<path>"}`.

use super::proposals;
use crate::kernel::KernelClient;
use crate::session::Session;
use crate::target::{self, Target};
use crate::util::{
    normalize_rel_path, now_rfc3339, resolve_in_repo, resolve_in_roots, tail_bytes, truncate_bytes,
};
use analyst_protocol::config::ProjectConfig;
use analyst_protocol::kernel::*;
use analyst_protocol::transcript::Event;
use analyst_protocol::TOOL_RESULT_CAP_BYTES;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(m: impl Into<String>) -> Self {
        ToolError { message: m.into() }
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError::new(format!("{e:#}"))
    }
}

impl From<crate::kernel::KernelError> for ToolError {
    fn from(e: crate::kernel::KernelError) -> Self {
        ToolError::new(e.to_string())
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(e: serde_json::Error) -> Self {
        ToolError::new(format!("serialization: {e}"))
    }
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        ToolError::new(e.to_string())
    }
}

/// A capped tool result as shown to the model.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub ok: bool,
    pub result: Value,
    pub truncated: bool,
}

/// Callback for live run output: `(stream, chunk)` with stream `stdout` or `stderr`.
pub type OutputSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Callback invoked with the run id as soon as `run.script` has started a run.
pub type RunStartedSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Everything tools need.
pub struct ToolContext {
    pub repo: PathBuf,
    pub home: PathBuf,
    pub session: Session,
    pub conv_id: String,
    pub kernel: Option<KernelClient>,
    pub target: Target,
    pub project: ProjectConfig,
    pub control_dir: Option<PathBuf>,
    pub last_sync_hash: Mutex<Option<String>>,
    pub output_sink: Option<OutputSink>,
    /// Told about every run this context starts (the desktop service attributes runs to
    /// conversations with it).
    pub on_run_started: Option<RunStartedSink>,
    /// User-approved external data directories (the global config's target `data_dirs`):
    /// `fs.read` may follow a repo symlink into them. Empty by default.
    pub data_roots: Vec<String>,
}

impl ToolContext {
    pub fn new(
        repo: &Path,
        home: &Path,
        session: Session,
        conv_id: &str,
        project: ProjectConfig,
    ) -> Self {
        ToolContext {
            repo: repo.to_path_buf(),
            home: home.to_path_buf(),
            session,
            conv_id: conv_id.to_string(),
            kernel: None,
            target: Target::Local,
            project,
            control_dir: None,
            last_sync_hash: Mutex::new(None),
            output_sink: None,
            on_run_started: None,
            data_roots: vec![],
        }
    }

    fn kernel(&self) -> Result<&KernelClient, ToolError> {
        self.kernel.as_ref().ok_or_else(|| {
            ToolError::new("the kernel is not connected on this target (target offline)")
        })
    }

    /// Execute a tool by internal id. Edit tools are built and applied directly here; the agent
    /// builds proposals separately when it needs approval first.
    pub async fn dispatch(
        &self,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<Value, ToolError> {
        match name {
            "fs.read" => self.fs_read(args),
            "fs.list" => self.fs_list(args),
            "fs.search" => self.fs_search(args).await,
            "scripts.index" => {
                let globs = Some(self.project.project.scripts.clone());
                Ok(serde_json::to_value(
                    self.kernel()?.scripts_index(globs).await?,
                )?)
            }
            "lib.index" => {
                let pkg = Some(self.project.project.library.clone());
                Ok(serde_json::to_value(self.kernel()?.lib_index(pkg).await?)?)
            }
            "runs.list" => {
                let limit = args.get("limit").and_then(Value::as_u64);
                match self.kernel() {
                    Ok(k) => Ok(serde_json::to_value(k.runs_list(limit).await?)?),
                    Err(_) => {
                        let mut runs = self.session.read_runs()?;
                        runs.reverse();
                        runs.truncate(limit.unwrap_or(20) as usize);
                        Ok(json!({"runs": runs, "source": "session"}))
                    }
                }
            }
            "runs.get" => {
                let id = str_arg(args, "run_id")?;
                match self.kernel() {
                    Ok(k) => Ok(serde_json::to_value(k.run_status(id).await?)?),
                    Err(_) => match self.session.find_run(id)? {
                        Some(r) => Ok(serde_json::to_value(r)?),
                        None => Err(ToolError::new(format!(
                            "run `{id}` not found in this session"
                        ))),
                    },
                }
            }
            "out.list" => {
                let p: OutListParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.out_list(p).await?)?)
            }
            "out.read" => {
                let p: OutReadParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.out_read(p).await?)?)
            }
            "eq.extract" => {
                let p: EqExtractParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.eq_extract(p).await?)?)
            }
            "eq.check" => {
                let p: EqCheckParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(self.kernel()?.eq_check(p).await?)?)
            }
            "contract.check" => {
                let p: PathsParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(
                    self.kernel()?.contract_check(p.paths).await?,
                )?)
            }
            "contract.similar" => {
                let p: SimilarParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(
                    self.kernel()?.contract_similar(p.threshold).await?,
                )?)
            }
            "lint.run" => {
                let p: PathsParams = serde_json::from_value(args.clone()).map_err(bad_args)?;
                Ok(serde_json::to_value(
                    self.kernel()?.lint_run(p.paths).await?,
                )?)
            }
            "fig.list" => self.fig_list(args),
            "target.info" => self.target_info().await,
            "fs.propose_edit" | "fs.propose_create" | "fs.propose_delete" => {
                let p = proposals::build(&self.repo, call_id, name, args)?;
                proposals::apply(&self.repo, &p)?;
                Ok(json!({"applied": true, "path": p.path, "summary": p.summary}))
            }
            "run.script" => self.run_script(args).await,
            "exec.snippet" => {
                let code = str_arg(args, "code")?;
                let t = args.get("timeout_s").and_then(Value::as_f64);
                Ok(serde_json::to_value(
                    self.kernel()?.exec_snippet(code, t).await?,
                )?)
            }
            "pkg.add" | "pkg.remove" | "pkg.sync" => self.pkg(name, args).await,
            "shell.run" => self.shell_run(args).await,
            "git.status" => {
                let out = crate::checkpoint::git(
                    &self.repo,
                    &["status", "--porcelain=v1", "--branch"],
                    None,
                )?;
                Ok(json!({"status": out}))
            }
            "git.diff" => {
                let mut a = vec!["diff", "--no-color"];
                let p;
                if let Some(path) = args.get("path").and_then(Value::as_str) {
                    p = normalize_rel_path(path).map_err(ToolError::new)?;
                    a.push("--");
                    a.push(&p);
                }
                let out = crate::checkpoint::git(&self.repo, &a, None)?;
                Ok(json!({"diff": out}))
            }
            "git.commit" => {
                let msg = str_arg(args, "message")?;
                crate::checkpoint::git(&self.repo, &["add", "-A"], None)?;
                let out = crate::checkpoint::git(&self.repo, &["commit", "-q", "-m", msg], None)?;
                let sha =
                    crate::checkpoint::git(&self.repo, &["rev-parse", "--short", "HEAD"], None)?;
                Ok(json!({"committed": true, "commit": sha.trim(), "output": out}))
            }
            "declare_scope" | "pin_fact" => Err(ToolError::new(format!(
                "`{name}` is handled by the agent loop, not dispatch"
            ))),
            other => Err(ToolError::new(format!("unknown tool `{other}`"))),
        }
    }

    /// Cap a result for the model. The cap applies to the final serialisation, for errors as
    /// well as successes. Larger results are spilled to the session artifacts directory as
    /// line-oriented text (string fields raw, everything else pretty JSON), so `fs.read` with
    /// a line range can page through them.
    pub fn cap_result(&self, call_id: &str, result: Result<Value, ToolError>) -> ToolOutcome {
        let (ok, value) = match result {
            Ok(v) => (true, v),
            Err(e) => (false, json!({"error": e.message})),
        };
        let text = serde_json::to_string(&value).unwrap_or_default();
        if text.len() <= TOOL_RESULT_CAP_BYTES {
            return ToolOutcome {
                ok,
                result: value,
                truncated: false,
            };
        }
        let full = render_for_paging(&value);
        let safe_id: String = call_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = format!("{}/{}.txt", self.conv_id, safe_id);
        let stored = self.session.spill_artifact(&name, full.as_bytes());
        let total_lines = full.lines().count();
        let mut head_len = TOOL_RESULT_CAP_BYTES * 3 / 4;
        loop {
            let head = truncate_bytes(&full, head_len);
            let mut out = json!({
                "head": head,
                "truncated": true,
                "total_bytes": full.len(),
                "total_lines": total_lines,
            });
            if !ok {
                out["error"] = json!(truncate_bytes(&full, head_len.min(2000)));
            }
            match &stored {
                Ok(path) => {
                    out["artifact"] = json!(path.display().to_string());
                    out["hint"] = json!(
                        "page the full result with fs.read on the artifact path using start_line/end_line"
                    );
                }
                Err(e) => out["spill_error"] = json!(format!("could not store full result: {e}")),
            }
            let len = serde_json::to_string(&out).map(|t| t.len()).unwrap_or(0);
            if len <= TOOL_RESULT_CAP_BYTES || head_len < 256 {
                return ToolOutcome {
                    ok,
                    result: out,
                    truncated: true,
                };
            }
            // JSON escaping can inflate the head well past its raw size.
            head_len = head_len * 2 / 3;
        }
    }

    // ---- filesystem ----

    /// Directories outside the repo that reads may reach through a symlink: the target's
    /// user-approved data directories (local paths only make sense on the local target).
    fn read_roots(&self) -> Vec<PathBuf> {
        if self.target.is_remote() {
            return vec![];
        }
        self.data_roots
            .iter()
            .map(|d| crate::util::expand_tilde(d))
            .filter(|d| d.is_absolute())
            .collect()
    }

    /// Resolve a path argument: repo-relative (canonicalised, so a symlink cannot lead out of
    /// the repo), or an absolute path inside the session artifacts directory (spilled results).
    fn resolve_read_path(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let p = Path::new(raw);
        if p.is_absolute() {
            let art = self.session.artifacts_dir();
            let canon =
                std::fs::canonicalize(p).map_err(|e| ToolError::new(format!("`{raw}`: {e}")))?;
            let art_canon = std::fs::canonicalize(&art).unwrap_or(art);
            if canon.starts_with(&art_canon) {
                return Ok(canon);
            }
            return Err(ToolError::new(format!(
                "`{raw}` is outside the repo and the session artifacts directory"
            )));
        }
        // The repo's `.env` holds API keys for the app. The agent's file tools never read
        // it, so a key cannot end up in a prompt.
        if normalize_rel_path(raw).is_ok_and(|n| n.eq_ignore_ascii_case(wb_net::secrets::ENV_FILE))
        {
            return Err(ToolError::new(
                "`.env` holds API keys and is not readable through the file tools",
            ));
        }
        resolve_in_roots(&self.repo, raw, true, &self.read_roots()).map_err(ToolError::new)
    }

    fn fs_read(&self, args: &Value) -> Result<Value, ToolError> {
        let raw = str_arg(args, "path")?;
        let full = self.resolve_read_path(raw)?;
        let bytes = std::fs::read(&full).map_err(|e| ToolError::new(format!("`{raw}`: {e}")))?;
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                return Ok(json!({"path": raw, "binary": true, "size": e.as_bytes().len()}));
            }
        };
        let total = text.lines().count();
        let start = args
            .get("start_line")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let end = args
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|e| e as usize)
            .unwrap_or(total)
            .min(total);
        // A single line can be enormous (minified data); bound each one so a page of lines
        // is a page.
        const MAX_LINE: usize = 2000;
        let content: String = if start > end {
            String::new()
        } else {
            text.lines()
                .enumerate()
                .filter(|(i, _)| *i + 1 >= start && *i < end)
                .map(|(i, l)| {
                    if l.len() > MAX_LINE {
                        format!(
                            "{:>5}| {}...[line truncated, {} bytes]",
                            i + 1,
                            truncate_bytes(l, MAX_LINE),
                            l.len()
                        )
                    } else {
                        format!("{:>5}| {l}", i + 1)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(
            json!({"path": raw, "start_line": start, "end_line": end, "total_lines": total, "content": content}),
        )
    }

    fn fs_list(&self, args: &Value) -> Result<Value, ToolError> {
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let canon_repo = std::fs::canonicalize(&self.repo)?;
        let depth = args
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .clamp(1, 6) as usize;
        let root = resolve_in_repo(&self.repo, raw, true).map_err(ToolError::new)?;
        if !root.is_dir() {
            return Err(ToolError::new(format!("`{raw}` is not a directory")));
        }
        let mut entries = Vec::new();
        for e in walkdir::WalkDir::new(&root)
            .min_depth(1)
            .max_depth(depth)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                let n = e.file_name().to_string_lossy();
                !(n == ".git"
                    || n == ".venv"
                    || n == "__pycache__"
                    || n == "node_modules"
                    || n == ".ruff_cache")
            })
            .flatten()
        {
            let p = e.path().strip_prefix(&canon_repo).unwrap_or(e.path());
            let mut s = p.to_string_lossy().replace('\\', "/");
            if e.file_type().is_dir() {
                s.push('/');
            }
            entries.push(s);
            if entries.len() >= 2000 {
                break;
            }
        }
        Ok(json!({"path": raw, "entries": entries}))
    }

    async fn fs_search(&self, args: &Value) -> Result<Value, ToolError> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
        let pattern = str_arg(args, "pattern")?;
        let max = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, 1000) as usize;
        let rg = crate::util::find_tool("rg").ok_or_else(|| {
            ToolError::new("ripgrep (`rg`) is not installed, so fs.search is unavailable")
        })?;
        let mut cmd = tokio::process::Command::new(rg);
        cmd.args(["--json", "--max-columns", "1000", "-e", pattern])
            .arg("--glob")
            .arg("!.git")
            .arg("--glob")
            .arg("!.venv")
            .current_dir(&self.repo)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(g) = args.get("glob").and_then(Value::as_str) {
            cmd.arg("--glob").arg(g);
        }
        cmd.arg("--").arg(".");
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::new(format!("running rg: {e}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::new("rg stdout not captured"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| ToolError::new("rg stderr not captured"))?;
        let err_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).to_string()
        });
        let mut matches = Vec::new();
        let mut truncated = false;
        let mut stopped_early = false;
        let read = async {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            let mut seen = 0usize;
            while let Ok(Some(line)) = lines.next_line().await {
                seen += line.len();
                if seen > MAX_STDOUT_BYTES {
                    truncated = true;
                    stopped_early = true;
                    break;
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v["type"] != "match" {
                    continue;
                }
                if matches.len() >= max {
                    truncated = true;
                    stopped_early = true;
                    break;
                }
                let d = &v["data"];
                let path = d["path"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .trim_start_matches("./");
                let text = d["lines"]["text"].as_str().unwrap_or("").trim_end();
                matches.push(json!({
                    "path": path,
                    "line": d["line_number"],
                    "text": truncate_bytes(text, 400),
                }));
            }
        };
        let timed_out = tokio::time::timeout(SEARCH_TIMEOUT, read).await.is_err();
        if timed_out || stopped_early {
            let _ = child.start_kill();
        }
        let status = child.wait().await.ok();
        let stderr_text = err_task.await.unwrap_or_default();
        if timed_out {
            return Err(ToolError::new(format!(
                "fs.search timed out after {} s; narrow the pattern or add a glob",
                SEARCH_TIMEOUT.as_secs()
            )));
        }
        // rg exits 0 with matches, 1 with none, 2 on errors (bad regex, unreadable files).
        // Killing it after enough matches is not an error.
        let code = status.and_then(|s| s.code());
        if !stopped_early && code != Some(0) && code != Some(1) {
            let msg = stderr_text.trim();
            if matches.is_empty() {
                return Err(ToolError::new(format!(
                    "ripgrep failed (this is not \"no matches\"): {}",
                    if msg.is_empty() {
                        format!("exit status {code:?}")
                    } else {
                        truncate_bytes(msg, 2000).to_string()
                    }
                )));
            }
            return Ok(json!({
                "pattern": pattern,
                "matches": matches,
                "truncated": truncated,
                "warning": truncate_bytes(msg, 1000),
            }));
        }
        Ok(json!({"pattern": pattern, "matches": matches, "truncated": truncated}))
    }

    fn fig_list(&self, args: &Value) -> Result<Value, ToolError> {
        let want = args.get("run_id").and_then(Value::as_str);
        let mut seen = std::collections::BTreeSet::new();
        let mut runs: Vec<_> = self.session.read_runs()?;
        runs.reverse();
        let mut figures = Vec::new();
        for r in runs {
            if !seen.insert(r.meta.run_id.clone()) {
                continue;
            }
            if let Some(w) = want {
                if r.meta.run_id != w {
                    continue;
                }
            }
            let dir = self.repo.join(&r.meta.out_dir);
            let session_dir = self.session.pulled_artifact_path(&r.meta.run_id, "");
            for base in [dir, session_dir] {
                if !base.is_dir() {
                    continue;
                }
                for e in walkdir::WalkDir::new(&base).into_iter().flatten() {
                    let p = e.path();
                    if !p.is_file() {
                        continue;
                    }
                    let rel = p.to_string_lossy().to_string();
                    if ArtifactKind::from_path(&rel) != ArtifactKind::Figure {
                        continue;
                    }
                    let sidecar = PathBuf::from(target::sync::sidecar_path(&rel));
                    let meta: Option<FigureSidecar> = std::fs::read_to_string(&sidecar)
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok());
                    figures.push(json!({
                        "run_id": r.meta.run_id,
                        "script": r.meta.script,
                        "path": p.strip_prefix(&self.repo).unwrap_or(p).to_string_lossy(),
                        "title": meta.as_ref().and_then(|m| m.title.clone()),
                        "axes": meta.as_ref().map(|m| m.axes.clone()),
                    }));
                }
            }
        }
        Ok(json!({"figures": figures}))
    }

    async fn target_info(&self) -> Result<Value, ToolError> {
        let hello = match &self.kernel {
            Some(k) => k.hello().await,
            None => None,
        };
        let connected = match &self.kernel {
            Some(k) => k.is_connected().await,
            None => false,
        };
        Ok(json!({
            "name": self.target.name(),
            "kind": if self.target.is_remote() { "ssh" } else { "local" },
            "host": self.target.host(),
            "connected": connected,
            "sandbox": hello.as_ref().map(|h| h.sandbox.clone()),
            "python": hello.as_ref().map(|h| h.python.clone()),
            "data_dirs": crate::config::effective_project(&self.project, self.target.name()).data_dirs,
            "target_data_dirs": self.target.data_dirs(),
        }))
    }

    // ---- runs ----

    /// Sync the mirror when the target is remote and the tree changed. Returns the tree hash.
    pub async fn sync_if_needed(&self) -> Result<Option<String>, ToolError> {
        if !self.target.is_remote() {
            return Ok(None);
        }
        let last = self.last_sync_hash.lock().await.clone();
        let report = target::sync::push(
            &self.repo,
            &self.target,
            self.control_dir.as_deref(),
            last.as_deref(),
            false,
        )
        .await?;
        if let Some(w) = &report.warning {
            tracing::warn!("{w}");
        }
        *self.last_sync_hash.lock().await = Some(report.tree_hash.clone());
        Ok(Some(report.tree_hash))
    }

    async fn run_script(&self, args: &Value) -> Result<Value, ToolError> {
        let script = str_arg(args, "script")?;
        let script = normalize_rel_path(script).map_err(ToolError::new)?;
        let run_args: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let eff = crate::config::effective_project(&self.project, self.target.name());
        let timeout_s = args
            .get("timeout_s")
            .and_then(Value::as_f64)
            .unwrap_or(eff.timeout_s as f64);
        let kernel = self.kernel()?.clone();
        let tree_hash = match self.sync_if_needed().await {
            Ok(h) => h,
            Err(e) => return Err(ToolError::new(format!("sync before run failed: {e}"))),
        };
        let tree_hash = match tree_hash {
            Some(h) => Some(h),
            None => target::sync::file_list(&self.repo)
                .ok()
                .map(|l| target::sync::tree_hash(&l)),
        };
        let mut rx = kernel.subscribe();
        let started = kernel
            .run_start(RunStartParams {
                script: script.clone(),
                args: Some(run_args.clone()),
                env: None,
                timeout_s: Some(timeout_s),
                tree_hash,
            })
            .await?;
        let run_id = started.run_id.clone();
        if let Some(sink) = &self.on_run_started {
            sink(&run_id);
        }
        // If this future is dropped before the run finishes (the turn was cancelled), the
        // guard cancels the run on the target.
        let cancel_guard = kernel.cancel_guard(&run_id);
        self.session.append_event(
            &self.conv_id,
            &Event::Run {
                ts: now_rfc3339(),
                run_id: run_id.clone(),
                script: script.clone(),
                args: run_args.clone(),
                target: self.target.name().to_string(),
            },
        )?;
        if let Ok(meta) = kernel.run_status(&run_id).await {
            let _ = self.session.append_run(&meta);
        }
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut artifacts: Vec<Artifact> = Vec::new();
        let mut done: Option<RunDone> = None;
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_s + 60.0);
        loop {
            let n = match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(n)) => n,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
                Err(_) => break,
            };
            if n.run_id() != Some(run_id.as_str()) {
                continue;
            }
            match n {
                KernelNotification::Stdout(o) => {
                    if let Some(s) = &self.output_sink {
                        s("stdout", &o.chunk);
                    }
                    push_bounded(&mut stdout, &o.chunk);
                }
                KernelNotification::Stderr(o) => {
                    if let Some(s) = &self.output_sink {
                        s("stderr", &o.chunk);
                    }
                    push_bounded(&mut stderr, &o.chunk);
                }
                KernelNotification::Artifact(a) => artifacts.push(a.artifact),
                KernelNotification::Done(d) => {
                    done = Some(d);
                    break;
                }
                KernelNotification::Other { .. } => {}
            }
        }
        let meta = kernel.run_status(&run_id).await.ok();
        if let Some(m) = &meta {
            let _ = self.session.append_run(m);
        }
        // Finished, or still running past the wait deadline (the agent may poll it): either
        // way this call no longer owns the run.
        cancel_guard.disarm();
        // Pull rendered artifacts (and only those) from a remote target, once: figures and
        // other rendered files, their sidecars, and run.json. Data stays on the host.
        let mut pull_error = None;
        if let (Target::Ssh { host, .. }, Some(mirror)) =
            (&self.target, self.target.mirror_dir(&self.repo))
        {
            let paths = target::sync::rendered_paths(&artifacts, &started.out_dir);
            match target::sync::pull_rendered(
                host,
                &mirror,
                &paths,
                &self.repo,
                self.control_dir.as_deref(),
            )
            .await
            {
                Err(e) => pull_error = Some(e.to_string()),
                Ok(()) => {
                    for rel in &paths {
                        let src = self.repo.join(rel);
                        if !src.is_file() {
                            continue;
                        }
                        let dst = self.session.pulled_artifact_path(&run_id, rel);
                        if let Some(p) = dst.parent() {
                            let _ = std::fs::create_dir_all(p);
                        }
                        let _ = std::fs::copy(&src, &dst);
                    }
                }
            }
        }
        let status = done
            .as_ref()
            .map(|d| d.status)
            .or(meta.as_ref().map(|m| m.status))
            .unwrap_or(RunStatus::Running);
        let exit_code = done
            .as_ref()
            .and_then(|d| d.exit_code)
            .or(meta.as_ref().and_then(|m| m.exit_code));
        let figures: Vec<&str> = artifacts
            .iter()
            .filter(|a| a.kind == ArtifactKind::Figure)
            .map(|a| a.path.as_str())
            .collect();
        let tail = |s: &str| tail_bytes(s, 6000);
        let run_id = run_id.clone();
        let mut result = json!({
            "run_id": run_id,
            "script": script,
            "args": run_args,
            "status": status,
            "exit_code": exit_code,
            "duration_s": done.as_ref().map(|d| d.duration_s),
            "out_dir": started.out_dir,
            "stdout": tail(&stdout),
            "stderr": tail(&stderr),
            "artifacts": artifacts.iter().map(|a| json!({"kind": a.kind, "path": a.path, "size": a.size})).collect::<Vec<_>>(),
            "figures": figures,
        });
        if status == RunStatus::Running {
            result["note"] =
                json!("the run did not finish before the wait deadline; use runs.get to poll it");
        } else if status != RunStatus::Done || exit_code.unwrap_or(0) != 0 {
            result["failed"] = json!(true);
            result["note"] =
                json!("the run failed; read stderr above, fix the cause, and run again");
        }
        if let Some(e) = pull_error {
            result["pull_error"] = json!(e);
        }
        Ok(result)
    }

    async fn pkg(&self, name: &str, args: &Value) -> Result<Value, ToolError> {
        let packages: Vec<String> = args
            .get("packages")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if name != "pkg.sync" && packages.is_empty() {
            return Err(ToolError::new("`packages` must list at least one package"));
        }
        validate_package_names(&packages)?;
        let has_pyproject = self.repo.join("pyproject.toml").is_file();
        let local = local_uv_line(name, has_pyproject, self.target.is_remote(), &packages)?;
        let (host, mirror) = match &self.target {
            Target::Local => {
                let out = target::run_command(&local, Some(&self.repo)).await?;
                return Ok(json!({
                    "ok": true,
                    "stdout": tail_bytes(&String::from_utf8_lossy(&out.stdout), 4000),
                    "stderr": tail_bytes(&String::from_utf8_lossy(&out.stderr), 4000),
                }));
            }
            Target::Ssh { host, .. } => (
                host.clone(),
                self.target.mirror_dir(&self.repo).unwrap_or_default(),
            ),
        };
        // Remote target: the local repo is canonical. Change pyproject.toml and the lockfile
        // locally, push them, then make the remote environment match with `uv sync`.
        // Editing only the mirror would be undone by the next push.
        let mut log = String::new();
        if has_pyproject {
            if name != "pkg.sync" {
                let out = target::run_command(&local, Some(&self.repo)).await?;
                log.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            self.sync_if_needed().await.map_err(|e| {
                ToolError::new(format!("sync after the package change failed: {e}"))
            })?;
        }
        let index_flag = crate::egress::remote_index_flag("uv (remote)")?;
        let remote = remote_uv_command(name, has_pyproject, &mirror, &packages, &index_flag);
        let s = target::run_ssh(&host, self.control_dir.as_deref(), &remote).await?;
        log.push_str(&s);
        Ok(json!({"ok": true, "output": tail_bytes(&log, 4000)}))
    }

    /// `shell.run` goes through the kernel's sandboxed REPL (`exec.snippet`) with a subprocess
    /// wrapper, since the kernel protocol has no dedicated shell method.
    async fn shell_run(&self, args: &Value) -> Result<Value, ToolError> {
        let command = str_arg(args, "command")?;
        let timeout = args
            .get("timeout_s")
            .and_then(Value::as_f64)
            .unwrap_or(120.0);
        let code = format!(
            "import subprocess as _sp, json as _json\n_r = _sp.run({cmd}, shell=True, capture_output=True, text=True, timeout={timeout})\nprint(_json.dumps({{'exit_code': _r.returncode, 'stdout': _r.stdout[-8000:], 'stderr': _r.stderr[-8000:]}}))\n",
            cmd = serde_json::to_string(command).unwrap_or_default(),
        );
        let r = self
            .kernel()?
            .exec_snippet(&code, Some(timeout + 5.0))
            .await?;
        if !r.ok {
            return Err(ToolError::new(format!(
                "shell wrapper failed: {}",
                tail_bytes(&r.stderr, 2000)
            )));
        }
        let last = r.stdout.lines().last().unwrap_or("");
        match serde_json::from_str::<Value>(last) {
            Ok(v) => Ok(v),
            Err(_) => Ok(json!({"stdout": r.stdout, "stderr": r.stderr})),
        }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new(format!("`{key}` (string) is required")))
}

fn bad_args(e: serde_json::Error) -> ToolError {
    ToolError::new(format!("invalid arguments: {e}"))
}

/// Keep at most the last 256 KiB of run output in memory; only a tail goes to the model.
fn push_bounded(buf: &mut String, chunk: &str) {
    const MAX: usize = 256 * 1024;
    buf.push_str(chunk);
    if buf.len() > MAX {
        let mut cut = buf.len() - MAX / 2;
        while cut < buf.len() && !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.drain(..cut);
    }
}

/// Package arguments are requirement specifiers, never options (A9).
pub fn validate_package_names(packages: &[String]) -> Result<(), ToolError> {
    for p in packages {
        if p.is_empty() {
            return Err(ToolError::new("empty package name"));
        }
        if p.starts_with('-') {
            return Err(ToolError::new(format!(
                "`{p}` is not a package name (it starts with `-` and would be read as an option)"
            )));
        }
        if p.chars().any(|c| c.is_control()) {
            return Err(ToolError::new(format!(
                "package name `{}` contains control characters",
                p.escape_debug()
            )));
        }
    }
    Ok(())
}

/// The `uv` command line run in the local repo. On a remote target with a pyproject, adds
/// and removals only update `pyproject.toml` and `uv.lock` (`--no-sync`).
///
/// Everything that resolves or downloads packages is built by [`crate::egress::uv_install`]
/// and so names the configured index; `pip uninstall` needs no index and runs offline.
pub fn local_uv_line(
    tool: &str,
    has_pyproject: bool,
    remote_target: bool,
    packages: &[String],
) -> Result<Vec<String>, ToolError> {
    let mut rest: Vec<String> = Vec::new();
    if has_pyproject && remote_target && tool != "pkg.sync" {
        rest.push("--no-sync".into());
    }
    if tool != "pkg.sync" {
        rest.push("--".into());
        rest.extend(packages.iter().cloned());
    }
    let install = |sub: &[&str]| crate::egress::uv_install("uv", sub, &rest);
    match (tool, has_pyproject) {
        ("pkg.add", true) => Ok(install(&["add"])),
        ("pkg.remove", true) => Ok(install(&["remove"])),
        ("pkg.add", false) => Ok(install(&["pip", "install"])),
        ("pkg.remove", false) => {
            let mut line: Vec<String> = ["uv", "pip", "uninstall"].map(String::from).to_vec();
            line.extend(rest.iter().cloned());
            Ok(line)
        }
        ("pkg.sync", true) => Ok(install(&["sync"])),
        ("pkg.sync", false) => Err(ToolError::new("pkg.sync needs a pyproject.toml / uv.lock")),
        _ => Err(ToolError::new(format!("unknown package tool `{tool}`"))),
    }
}

/// The remote shell command applied in the mirror after the push. `index_flag` is the
/// authorized `--default-index '<url>'` from [`crate::egress::remote_index_flag`]; a removal
/// without a pyproject needs no index and runs offline.
pub fn remote_uv_command(
    tool: &str,
    has_pyproject: bool,
    mirror: &str,
    packages: &[String],
    index_flag: &str,
) -> String {
    let cd = format!("cd {}", crate::util::sh_quote_path(mirror));
    if has_pyproject {
        return format!("{cd} && ~/.analyst/bin/uv sync {index_flag}");
    }
    let pkgs: Vec<String> = packages.iter().map(|p| crate::util::sh_quote(p)).collect();
    if tool == "pkg.remove" {
        format!(
            "{cd} && UV_OFFLINE=1 ~/.analyst/bin/uv pip uninstall -- {}",
            pkgs.join(" ")
        )
    } else {
        format!(
            "{cd} && ~/.analyst/bin/uv pip install {index_flag} -- {}",
            pkgs.join(" ")
        )
    }
}

/// Line-oriented rendering of a tool result for the spilled artifact: top-level string
/// fields are written raw under a `## <key>` heading so their own lines can be paged.
fn render_for_paging(v: &Value) -> String {
    fn one(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        }
    }
    match v {
        Value::Object(m) => {
            let mut out = String::new();
            for (k, val) in m {
                out.push_str(&format!("## {k}\n{}\n", one(val)));
            }
            out
        }
        other => one(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        crate::checkpoint::git(repo.path(), &["init", "-q"], None).unwrap();
        std::fs::create_dir_all(repo.path().join("scripts")).unwrap();
        std::fs::write(
            repo.path().join("scripts/a.py"),
            "import argparse\n\ndef main():\n    pass\n",
        )
        .unwrap();
        let session = Session::create(home.path(), repo.path(), "local", "p/m").unwrap();
        let c = ToolContext::new(
            repo.path(),
            home.path(),
            session,
            "c1",
            ProjectConfig::default(),
        );
        (home, repo, c)
    }

    #[tokio::test]
    async fn fs_tools_and_capping() {
        let (_h, _r, c) = ctx();
        let r = c
            .dispatch(
                "1",
                "fs.read",
                &json!({"path":"scripts/a.py","start_line":3}),
            )
            .await
            .unwrap();
        assert_eq!(r["total_lines"], 4);
        assert!(r["content"]
            .as_str()
            .unwrap()
            .starts_with("    3| def main"));
        // The repo's `.env` (API keys) is not readable through the file tools.
        std::fs::write(c.repo.join(".env"), "SOME_API_KEY=sk-in-repo\n").unwrap();
        for path in [".env", "./.env", ".ENV"] {
            let e = c
                .dispatch("e", "fs.read", &json!({ "path": path }))
                .await
                .unwrap_err();
            assert!(e.message.contains("API keys"), "{}", e.message);
        }
        std::fs::remove_file(c.repo.join(".env")).unwrap();
        let l = c.dispatch("2", "fs.list", &json!({})).await.unwrap();
        let entries = l["entries"].as_array().unwrap();
        assert!(entries.iter().any(|e| e == "scripts/a.py"));
        assert!(!entries
            .iter()
            .any(|e| e.as_str().unwrap().starts_with(".git")));
        let s = c
            .dispatch("3", "fs.search", &json!({"pattern":"def main"}))
            .await
            .unwrap();
        assert_eq!(s["matches"][0]["path"], "scripts/a.py");
        assert_eq!(s["matches"][0]["line"], 3);
        assert!(c
            .dispatch("4", "fs.read", &json!({"path":"../etc/passwd"}))
            .await
            .is_err());
        assert!(c
            .dispatch("5", "scripts.index", &json!({}))
            .await
            .unwrap_err()
            .message
            .contains("not connected"));

        // A6: a long multi-line string must be pageable by line from the artifact.
        let long: String = (1..=3000)
            .map(|i| format!("row {i} \"quoted\"\n"))
            .collect();
        let big = json!({"stdout": long, "n": 3000});
        let o = c.cap_result("big", Ok(big));
        assert!(o.truncated);
        assert_eq!(o.result["truncated"], true);
        assert!(
            serde_json::to_string(&o.result).unwrap().len() <= TOOL_RESULT_CAP_BYTES,
            "the cap applies to the final serialisation"
        );
        let art = o.result["artifact"].as_str().unwrap().to_string();
        assert!(Path::new(&art).is_file());
        assert!(o.result["total_lines"].as_u64().unwrap() > 3000);
        // The artifact is readable through fs.read by absolute path, line by line, well
        // past the point where the head was cut.
        let page = c
            .dispatch(
                "6",
                "fs.read",
                &json!({"path": art, "start_line": 2900, "end_line": 2901}),
            )
            .await
            .unwrap();
        let content = page["content"].as_str().unwrap();
        assert!(content.contains("row 289"), "{content}");
        assert_eq!(content.lines().count(), 2);
        // Error results are capped too.
        let huge_err = c.cap_result("err", Err(ToolError::new("e".repeat(100_000))));
        assert!(!huge_err.ok && huge_err.truncated);
        assert!(serde_json::to_string(&huge_err.result).unwrap().len() <= TOOL_RESULT_CAP_BYTES);
        assert!(huge_err.result["error"]
            .as_str()
            .unwrap()
            .starts_with("## error"));
        let small = c.cap_result("s", Ok(json!({"a":1})));
        assert!(!small.truncated);
        let err = c.cap_result("e", Err(ToolError::new("nope")));
        assert!(!err.ok);
        assert_eq!(err.result["error"], "nope");
    }

    #[tokio::test]
    async fn edit_tools_apply_and_git_tools_work() {
        let (_h, r, c) = ctx();
        c.dispatch(
            "1",
            "fs.propose_create",
            &json!({"path":"analysis/new.py","content":"x = 1\n"}),
        )
        .await
        .unwrap();
        assert!(r.path().join("analysis/new.py").is_file());
        c.dispatch(
            "2",
            "fs.propose_edit",
            &json!({"path":"analysis/new.py","edits":[{"old":"x = 1","new":"x = 2"}]}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(r.path().join("analysis/new.py")).unwrap(),
            "x = 2\n"
        );
        let st = c.dispatch("3", "git.status", &json!({})).await.unwrap();
        assert!(st["status"].as_str().unwrap().contains("analysis/"));
        let cm = c
            .dispatch("4", "git.commit", &json!({"message":"m"}))
            .await
            .unwrap();
        assert_eq!(cm["committed"], true);
        c.dispatch("5", "fs.propose_delete", &json!({"path":"analysis/new.py"}))
            .await
            .unwrap();
        assert!(!r.path().join("analysis/new.py").exists());
        let d = c.dispatch("6", "git.diff", &json!({})).await.unwrap();
        assert!(d["diff"].as_str().unwrap().contains("x = 2"));
    }

    /// A5: an invalid regex is an error, not "no matches".
    #[tokio::test]
    async fn search_reports_ripgrep_errors() {
        let (_h, _r, c) = ctx();
        let err = c
            .dispatch("1", "fs.search", &json!({"pattern": "def main("}))
            .await
            .unwrap_err();
        assert!(err.message.contains("ripgrep failed"), "{}", err.message);
        assert!(err.message.contains("regex"), "{}", err.message);
        let none = c
            .dispatch(
                "2",
                "fs.search",
                &json!({"pattern": "no_such_symbol_anywhere"}),
            )
            .await
            .unwrap();
        assert_eq!(none["matches"].as_array().unwrap().len(), 0);
        assert_eq!(none["truncated"], false);
        // Output is bounded by max_results.
        let many: String = (0..500).map(|i| format!("needle {i}\n")).collect();
        std::fs::write(c.repo.join("many.txt"), many).unwrap();
        let some = c
            .dispatch(
                "3",
                "fs.search",
                &json!({"pattern": "needle", "max_results": 7}),
            )
            .await
            .unwrap();
        assert_eq!(some["matches"].as_array().unwrap().len(), 7);
        assert_eq!(some["truncated"], true);
    }

    /// G7: symlinks cannot take read, list, or write outside the repo.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_do_not_escape_the_repo() {
        use std::os::unix::fs::symlink;
        let (_h, r, mut c) = ctx();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "TOP SECRET\n").unwrap();
        symlink(outside.path(), r.path().join("esc")).unwrap();
        symlink(outside.path().join("secret.txt"), r.path().join("s.txt")).unwrap();
        for path in ["esc/secret.txt", "s.txt"] {
            let e = c
                .dispatch("1", "fs.read", &json!({"path": path}))
                .await
                .unwrap_err();
            assert!(
                e.message.contains("outside the repository"),
                "{}",
                e.message
            );
        }
        assert!(c
            .dispatch("2", "fs.list", &json!({"path": "esc"}))
            .await
            .is_err());
        assert!(c
            .dispatch(
                "3",
                "fs.propose_create",
                &json!({"path": "esc/new.txt", "content": "x"})
            )
            .await
            .is_err());
        assert!(c
            .dispatch(
                "4",
                "fs.propose_edit",
                &json!({"path": "s.txt", "content": "x"})
            )
            .await
            .is_err());
        assert!(!outside.path().join("new.txt").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
            "TOP SECRET\n"
        );
        // An approved data directory is readable through the link, still not writable.
        c.data_roots = vec![outside.path().to_string_lossy().to_string()];
        let ok = c
            .dispatch("5", "fs.read", &json!({"path": "esc/secret.txt"}))
            .await
            .unwrap();
        assert!(ok["content"].as_str().unwrap().contains("TOP SECRET"));
        assert!(c
            .dispatch(
                "6",
                "fs.propose_create",
                &json!({"path": "esc/new.txt", "content": "x"})
            )
            .await
            .is_err());
    }

    /// G4: the edit tools refuse reserved directories even when called directly.
    #[tokio::test]
    async fn reserved_directories_are_not_writable_through_dispatch() {
        let (_h, r, c) = ctx();
        let before = std::fs::read_to_string(r.path().join(".git/config")).unwrap();
        for (tool, args) in [
            (
                "fs.propose_edit",
                json!({"path": ".git/config", "content": "[diff]\n\texternal = touch pwned\n"}),
            ),
            (
                "fs.propose_create",
                json!({"path": ".git/hooks/pre-commit", "content": "#!/bin/sh\n"}),
            ),
            (
                "fs.propose_create",
                json!({"path": ".analyst/project.toml", "content": "[policy]\nedits = \"auto\"\n"}),
            ),
        ] {
            let e = c.dispatch("1", tool, &args).await.unwrap_err();
            assert!(e.message.contains("never editable"), "{}", e.message);
        }
        assert_eq!(
            std::fs::read_to_string(r.path().join(".git/config")).unwrap(),
            before
        );
    }

    /// A9 and R5: package arguments and the remote package flow.
    #[tokio::test]
    async fn package_command_construction() {
        let (_h, _r, c) = ctx();
        for bad in ["--index-url=http://evil", "-e", ""] {
            let e = c
                .dispatch("1", "pkg.add", &json!({"packages": [bad]}))
                .await
                .unwrap_err();
            assert!(e.message.contains("package name"), "{}", e.message);
        }
        let pk = vec!["numpy>=2".to_string(), "pandas".to_string()];
        // Installs name the configured index right after the subcommand; the gate
        // authorizes exactly that index when the line is spawned.
        let index = crate::egress::package_index();
        let ix = index.as_str();
        assert_eq!(
            local_uv_line("pkg.add", true, false, &pk).unwrap(),
            vec![
                "uv",
                "add",
                "--default-index",
                ix,
                "--",
                "numpy>=2",
                "pandas"
            ]
        );
        assert_eq!(
            local_uv_line("pkg.add", true, true, &pk).unwrap(),
            vec![
                "uv",
                "add",
                "--default-index",
                ix,
                "--no-sync",
                "--",
                "numpy>=2",
                "pandas"
            ]
        );
        assert_eq!(
            local_uv_line("pkg.add", false, false, &pk).unwrap(),
            vec![
                "uv",
                "pip",
                "install",
                "--default-index",
                ix,
                "--",
                "numpy>=2",
                "pandas"
            ]
        );
        // An uninstall needs no index: no flag, so it runs offline.
        let uninstall = local_uv_line("pkg.remove", false, false, &pk).unwrap();
        assert_eq!(
            uninstall,
            vec!["uv", "pip", "uninstall", "--", "numpy>=2", "pandas"]
        );
        assert!(crate::egress::guard(&uninstall)
            .unwrap()
            .env
            .contains(&("UV_OFFLINE".to_string(), "1".to_string())));
        assert_eq!(
            local_uv_line("pkg.sync", true, true, &[]).unwrap(),
            vec!["uv", "sync", "--default-index", ix]
        );
        assert!(local_uv_line("pkg.sync", false, false, &[]).is_err());
        // With a pyproject the remote side only syncs to the pushed lockfile, from the
        // same index.
        let flag = "--default-index 'https://pypi.org/simple'";
        assert_eq!(
            remote_uv_command("pkg.add", true, "~/analyst/my repo-1a2b3c4d", &pk, flag),
            "cd ~/'analyst/my repo-1a2b3c4d' && ~/.analyst/bin/uv sync --default-index 'https://pypi.org/simple'"
        );
        assert_eq!(
            remote_uv_command("pkg.add", false, "~/analyst/r", &pk, flag),
            "cd ~/analyst/r && ~/.analyst/bin/uv pip install --default-index 'https://pypi.org/simple' -- 'numpy>=2' pandas"
        );
        assert_eq!(
            remote_uv_command("pkg.remove", false, "~/analyst/r", &pk, flag),
            "cd ~/analyst/r && UV_OFFLINE=1 ~/.analyst/bin/uv pip uninstall -- 'numpy>=2' pandas"
        );
    }
}
