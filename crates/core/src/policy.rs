//! Policy engine (ARCHITECTURE 5.8): `(tool, args) -> Allow | Ask | Deny` from the project
//! policy table, the conversation mode, protected globs, the declared edit scope, and
//! `# analyst: lock` / `# analyst: unlock` protected regions.

use crate::tools::registry::{tool_class, ToolClass};
use analyst_protocol::config::{
    AskPolicy, EditPolicy, NetworkPolicy, PolicyConfig, ProjectConfig, RunPolicy, UnsandboxedRuns,
};
use analyst_protocol::evals::Mode;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub decision: Decision,
    pub reason: String,
}

impl Evaluation {
    fn allow(reason: impl Into<String>) -> Self {
        Evaluation {
            decision: Decision::Allow,
            reason: reason.into(),
        }
    }
    fn ask(reason: impl Into<String>) -> Self {
        Evaluation {
            decision: Decision::Ask,
            reason: reason.into(),
        }
    }
    fn deny(reason: impl Into<String>) -> Self {
        Evaluation {
            decision: Decision::Deny,
            reason: reason.into(),
        }
    }
}

pub const LOCK_MARKER: &str = "analyst: lock";
pub const UNLOCK_MARKER: &str = "analyst: unlock";

/// Policy state for one conversation.
#[derive(Debug, Clone)]
pub struct Policy {
    pub config: PolicyConfig,
    pub mode: Mode,
    repo: PathBuf,
    protected: GlobSet,
    /// Files the agent declared it intends to touch (`declare_scope`). `None` = not declared.
    /// Reset at the start of every turn ([`Policy::begin_turn`]).
    pub scope: Option<Vec<String>>,
    /// Sandbox kind reported by `kernel.hello` (`bwrap`, `sandbox-exec`, `limits`, `none`);
    /// `None` until [`Policy::set_sandbox`] is called, which is treated as unsandboxed.
    sandbox: Option<String>,
    /// `policy.unsandboxed_runs` from the project file.
    pub unsandboxed_runs: UnsandboxedRuns,
}

/// What a `declare_scope` call would do to the scope in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeChange {
    /// No scope yet this turn: free.
    First,
    /// Same files or fewer: free.
    Narrows,
    /// Adds these files to an existing scope: needs approval.
    Widens(Vec<String>),
}

/// Sandbox kinds that actually confine a run. Everything else (`limits`, `none`, unknown,
/// not yet reported) leaves the filesystem and network open.
pub fn sandbox_confines(kind: &str) -> bool {
    matches!(kind, "bwrap" | "sandbox-exec" | "landlock")
}

impl Policy {
    pub fn new(repo: &Path, config: PolicyConfig, mode: Mode) -> anyhow::Result<Policy> {
        // Probe once: on a case-insensitive volume `Analysis/Core/x.py` is the protected file.
        let case_insensitive = crate::util::fs_case_insensitive(repo);
        let mut b = GlobSetBuilder::new();
        for g in &config.protected {
            b.add(
                GlobBuilder::new(g)
                    .case_insensitive(case_insensitive)
                    .build()
                    .map_err(|e| anyhow::anyhow!("protected glob `{g}`: {e}"))?,
            );
        }
        let protected = b.build()?;
        Ok(Policy {
            config,
            mode,
            repo: repo.to_path_buf(),
            protected,
            scope: None,
            sandbox: None,
            unsandboxed_runs: UnsandboxedRuns::Ask,
        })
    }

    /// A policy from the whole project config (includes `policy.unsandboxed_runs`).
    pub fn for_project(repo: &Path, project: &ProjectConfig, mode: Mode) -> anyhow::Result<Policy> {
        let mut p = Policy::new(repo, project.policy.clone(), mode)?;
        p.unsandboxed_runs = project.unsandboxed_runs;
        Ok(p)
    }

    /// Record the sandbox kind the kernel reported in `kernel.hello`. Must be called before
    /// the first tool call; until then runs are treated as unsandboxed.
    pub fn set_sandbox(&mut self, kind: &str) {
        self.sandbox = Some(kind.to_string());
    }

    pub fn set_unsandboxed_runs(&mut self, v: UnsandboxedRuns) {
        self.unsandboxed_runs = v;
    }

    /// The sandbox kind last reported, if any.
    pub fn sandbox(&self) -> Option<&str> {
        self.sandbox.as_deref()
    }

    /// Whether runs are confined by an OS sandbox.
    pub fn is_sandboxed(&self) -> bool {
        self.sandbox
            .as_deref()
            .map(sandbox_confines)
            .unwrap_or(false)
    }

    /// Start of a turn: the declared scope does not carry over.
    pub fn begin_turn(&mut self) {
        self.scope = None;
    }

    fn normalize_scope(files: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in files {
            if let Ok(n) = crate::util::normalize_rel_path(f) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// Classify a declaration against the scope in force, without changing anything.
    pub fn scope_change(&self, files: &[String]) -> ScopeChange {
        let new = Self::normalize_scope(files);
        match &self.scope {
            None => ScopeChange::First,
            Some(cur) => {
                let added: Vec<String> = new.into_iter().filter(|f| !cur.contains(f)).collect();
                if added.is_empty() {
                    ScopeChange::Narrows
                } else {
                    ScopeChange::Widens(added)
                }
            }
        }
    }

    /// Replace the scope. Callers gate widening through [`Policy::scope_change`] and the
    /// approver first; this method does not ask.
    pub fn declare_scope(&mut self, files: Vec<String>) {
        self.scope = Some(Self::normalize_scope(&files));
    }

    pub fn is_protected(&self, path: &str) -> bool {
        self.protected.is_match(path)
    }

    /// Whether a (normalised) path is in the scope declared this turn.
    pub fn in_scope(&self, path: &str) -> bool {
        match &self.scope {
            None => false,
            Some(s) => s.iter().any(|f| f == path),
        }
    }

    /// Whether the mode makes a tool available at all.
    pub fn tool_available(&self, tool: &str) -> bool {
        let Some(class) = tool_class(tool) else {
            return false;
        };
        match self.mode {
            Mode::Build => true,
            Mode::Plan => matches!(class, ToolClass::Read | ToolClass::Meta),
            Mode::Run => {
                matches!(class, ToolClass::Read | ToolClass::Meta)
                    || matches!(tool, "run.script" | "exec.snippet")
            }
        }
    }

    /// One-paragraph summary for the system prompt.
    pub fn summary(&self) -> String {
        let c = &self.config;
        let edits = match c.edits {
            EditPolicy::Review => "every edit pauses for review",
            EditPolicy::AutoNewFiles => {
                "new files are written immediately, edits to existing files pause for review"
            }
            EditPolicy::Auto => "in-scope edits apply immediately; out-of-scope edits pause",
        };
        let runs = match c.runs {
            RunPolicy::Auto => "runs start without asking",
            RunPolicy::Ask => "runs ask first",
        };
        let ask = |p: AskPolicy| match p {
            AskPolicy::Ask => "ask",
            AskPolicy::Auto => "auto",
            AskPolicy::Deny => "denied",
        };
        let net = match c.network {
            NetworkPolicy::Deny => "denied",
            NetworkPolicy::Ask => "ask",
            NetworkPolicy::Allow => "allowed",
        };
        let mut s = format!(
            "Mode: {}. Edits: {edits}. Runs: {runs}. Packages: {}. Shell: {}. Git commits: {}. Network for runs: {net}. Max steps per turn: {}.",
            self.mode,
            ask(c.packages),
            ask(c.shell),
            ask(c.git),
            c.max_steps
        );
        if !c.protected.is_empty() {
            s.push_str(&format!(
                " Protected (never editable): {}.",
                c.protected.join(", ")
            ));
        }
        s.push_str(" Paths under .git/ and .analyst/ are never editable.");
        let kind = self.sandbox.as_deref().unwrap_or("unknown");
        if self.is_sandboxed() {
            s.push_str(&format!(" Sandbox: {kind} (runs are confined)."));
        } else {
            match self.unsandboxed_runs {
                UnsandboxedRuns::Ask => s.push_str(&format!(
                    " Sandbox: {kind}: runs are unsandboxed, so every run, snippet, and shell command asks the user first regardless of the runs policy."
                )),
                UnsandboxedRuns::Auto => s.push_str(&format!(
                    " Sandbox: {kind}: runs are unsandboxed (the project allows this without asking)."
                )),
            }
        }
        match &self.scope {
            Some(files) if !files.is_empty() => s.push_str(&format!(
                " Declared scope: {} (this turn only; adding files later needs approval).",
                files.join(", ")
            )),
            _ if self.mode == Mode::Build => s.push_str(
                " No scope declared yet this turn (scope resets every turn): call declare_scope once with every file you intend to touch before editing, or every edit will need approval. Declaring again to add files needs user approval.",
            ),
            _ => {}
        }
        s
    }

    /// Evaluate a tool call.
    pub fn evaluate(&self, tool: &str, args: &Value) -> Evaluation {
        let Some(class) = tool_class(tool) else {
            return Evaluation::deny(format!("unknown tool `{tool}`"));
        };
        if !self.tool_available(tool) {
            return Evaluation::deny(format!(
                "tool `{tool}` is not available in {} mode",
                self.mode
            ));
        }
        match class {
            ToolClass::Read | ToolClass::Meta => Evaluation::allow("read-only"),
            ToolClass::Edit => self.evaluate_edit(tool, args),
            ToolClass::Run => self.unsandboxed_gate(tool, self.evaluate_run(args)),
            ToolClass::Package => Self::from_ask(self.config.packages, "packages"),
            ToolClass::Shell => {
                self.unsandboxed_gate(tool, Self::from_ask(self.config.shell, "shell"))
            }
            ToolClass::Git => Self::from_ask(self.config.git, "git"),
        }
    }

    /// Run- and shell-class tools ask when nothing confines them (G2), unless the project
    /// opted out. A denial stays a denial.
    fn unsandboxed_gate(&self, tool: &str, base: Evaluation) -> Evaluation {
        if base.decision == Decision::Deny
            || self.is_sandboxed()
            || self.unsandboxed_runs == UnsandboxedRuns::Auto
        {
            return base;
        }
        let kind = self.sandbox.as_deref().unwrap_or("unknown");
        Evaluation::ask(format!(
            "`{tool}` would run unsandboxed (kernel sandbox: {kind}): it can read and write anything your account can, and network limits are not enforced"
        ))
    }

    fn from_ask(p: AskPolicy, what: &str) -> Evaluation {
        match p {
            AskPolicy::Auto => Evaluation::allow(format!("{what} policy is auto")),
            AskPolicy::Ask => Evaluation::ask(format!("{what} policy is ask")),
            AskPolicy::Deny => Evaluation::deny(format!("{what} policy is deny")),
        }
    }

    fn evaluate_run(&self, args: &Value) -> Evaluation {
        let wants_network = args
            .get("network")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if wants_network {
            match self.config.network {
                NetworkPolicy::Deny => {
                    return Evaluation::deny("network access is denied by policy")
                }
                NetworkPolicy::Ask => return Evaluation::ask("network access requires approval"),
                NetworkPolicy::Allow => {}
            }
        }
        match self.config.runs {
            RunPolicy::Auto => Evaluation::allow("runs policy is auto"),
            RunPolicy::Ask => Evaluation::ask("runs policy is ask"),
        }
    }

    fn evaluate_edit(&self, tool: &str, args: &Value) -> Evaluation {
        let raw = args.get("path").and_then(Value::as_str).unwrap_or("");
        let path = match crate::util::normalize_rel_path(raw) {
            Ok(p) => p,
            Err(e) => return Evaluation::deny(e),
        };
        if crate::util::is_reserved_path(&path) {
            return Evaluation::deny(format!(
                "`{path}` is under .git/ or .analyst/, which are never editable"
            ));
        }
        if self.is_protected(&path) {
            return Evaluation::deny(format!("`{path}` matches a protected pattern"));
        }
        let full = match crate::util::resolve_in_repo(&self.repo, &path, false) {
            Ok(f) => f,
            Err(e) => return Evaluation::deny(e),
        };
        // The link target may be protected or reserved even when the given name is not.
        if let Ok(canon_root) = std::fs::canonicalize(&self.repo) {
            if let Ok(real_rel) = full.strip_prefix(&canon_root) {
                let real = real_rel.to_string_lossy().replace('\\', "/");
                if !real.is_empty()
                    && (crate::util::is_reserved_path(&real) || self.is_protected(&real))
                {
                    return Evaluation::deny(format!(
                        "`{path}` resolves to `{real}`, which is protected"
                    ));
                }
            }
        }
        // Protected regions inside the existing file, checked against the edit's result.
        if tool != "fs.propose_create" {
            if let Ok(existing) = std::fs::read_to_string(&full) {
                if let Some(reason) = edit_touches_locked_region(tool, &existing, args) {
                    return Evaluation::deny(reason);
                }
            }
        }
        let is_new = tool == "fs.propose_create";
        match self.config.edits {
            EditPolicy::Review => Evaluation::ask("edits policy is review"),
            EditPolicy::AutoNewFiles if is_new => {
                Evaluation::allow("new file under auto-new-files")
            }
            EditPolicy::AutoNewFiles => Evaluation::ask("existing file under auto-new-files"),
            EditPolicy::Auto => {
                if self.in_scope(&path) {
                    Evaluation::allow("in declared scope under auto")
                } else if self.scope.is_none() {
                    Evaluation::ask("no scope declared; edit requires approval")
                } else {
                    Evaluation::ask(format!("`{path}` is outside the declared scope"))
                }
            }
        }
    }
}

/// Locked line ranges (1-based, inclusive, markers included). An unterminated lock runs to EOF.
pub fn locked_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start: Option<usize> = None;
    let mut last = 0;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        last = n;
        let t = line.trim();
        let is_comment = t.starts_with('#') || t.starts_with("//") || t.starts_with("--");
        if !is_comment {
            continue;
        }
        if t.contains(UNLOCK_MARKER) {
            if let Some(s) = start.take() {
                ranges.push((s, n));
            }
        } else if t.contains(LOCK_MARKER) && start.is_none() {
            start = Some(n);
        }
    }
    if let Some(s) = start {
        ranges.push((s, last.max(s)));
    }
    ranges
}

/// Text of each locked block, verbatim.
pub fn locked_blocks(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    locked_ranges(text)
        .into_iter()
        .map(|(s, e)| lines[s - 1..e.min(lines.len())].join("\n"))
        .collect()
}

fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Whether every locked block of `original` survives unchanged, in order, as a locked block
/// of `result`.
pub fn locked_blocks_preserved(original: &str, result: &str) -> bool {
    let before = locked_blocks(&lf(original));
    let after = locked_blocks(&lf(result));
    let mut it = after.iter();
    before.iter().all(|b| it.any(|a| a == b))
}

/// Returns a denial reason when an edit proposal would modify a locked region. The check is
/// made against the text the edit produces, so no sequence of edits can get around it.
pub fn edit_touches_locked_region(tool: &str, existing: &str, args: &Value) -> Option<String> {
    let ranges = locked_ranges(existing);
    if ranges.is_empty() {
        return None;
    }
    if tool == "fs.propose_delete" {
        return Some(
            "file contains a locked region (`# analyst: lock`) and cannot be deleted".into(),
        );
    }
    let result = if let Some(edits) = args.get("edits").and_then(Value::as_array) {
        // An edit list that does not apply fails later with its own error.
        crate::tools::proposals::apply_edits(existing, edits).ok()?
    } else {
        args.get("content").and_then(Value::as_str)?.to_string()
    };
    if locked_blocks_preserved(existing, &result) {
        return None;
    }
    let (s, e) = ranges[0];
    Some(format!(
        "the edit would change a locked region (`# analyst: lock`, first at lines {s}-{e}); locked regions must stay byte-for-byte identical"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(edits: EditPolicy, mode: Mode, repo: &Path) -> Policy {
        let cfg = PolicyConfig {
            edits,
            protected: vec!["data/**".into()],
            ..Default::default()
        };
        let mut p = Policy::new(repo, cfg, mode).unwrap();
        p.set_sandbox("bwrap");
        p
    }

    #[test]
    fn read_tools_allowed_everywhere_edits_follow_table() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy(EditPolicy::Review, Mode::Build, dir.path());
        assert_eq!(
            p.evaluate("fs.read", &json!({"path":"a"})).decision,
            Decision::Allow
        );
        assert_eq!(
            p.evaluate(
                "fs.propose_edit",
                &json!({"path":"scripts/a.py","content":"x"})
            )
            .decision,
            Decision::Ask
        );
        assert_eq!(
            p.evaluate(
                "fs.propose_create",
                &json!({"path":"data/x.csv","content":"x"})
            )
            .decision,
            Decision::Deny
        );
        assert_eq!(p.evaluate("pkg.add", &json!({})).decision, Decision::Ask);
        assert_eq!(
            p.evaluate("run.script", &json!({"script":"s"})).decision,
            Decision::Allow
        );
        assert_eq!(
            p.evaluate("run.script", &json!({"script":"s","network":true}))
                .decision,
            Decision::Deny
        );
        assert_eq!(p.evaluate("nope", &json!({})).decision, Decision::Deny);
    }

    #[test]
    fn modes_restrict_tool_sets() {
        let dir = tempfile::tempdir().unwrap();
        let plan = policy(EditPolicy::Auto, Mode::Plan, dir.path());
        assert_eq!(
            plan.evaluate("fs.propose_create", &json!({"path":"a.py","content":""}))
                .decision,
            Decision::Deny
        );
        assert_eq!(
            plan.evaluate("run.script", &json!({})).decision,
            Decision::Deny
        );
        let run = policy(EditPolicy::Auto, Mode::Run, dir.path());
        assert_eq!(
            run.evaluate("run.script", &json!({})).decision,
            Decision::Allow
        );
        assert_eq!(
            run.evaluate("exec.snippet", &json!({})).decision,
            Decision::Allow
        );
        assert_eq!(run.evaluate("pkg.add", &json!({})).decision, Decision::Deny);
        assert_eq!(
            run.evaluate("fs.propose_edit", &json!({"path":"a.py"}))
                .decision,
            Decision::Deny
        );
    }

    #[test]
    fn auto_mode_respects_scope() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = policy(EditPolicy::Auto, Mode::Build, dir.path());
        let args = json!({"path":"scripts/a.py","content":"x"});
        assert_eq!(p.evaluate("fs.propose_edit", &args).decision, Decision::Ask);
        p.declare_scope(vec!["scripts/a.py".into()]);
        assert_eq!(
            p.evaluate("fs.propose_edit", &args).decision,
            Decision::Allow
        );
        let other = json!({"path":"scripts/b.py","content":"x"});
        let ev = p.evaluate("fs.propose_edit", &other);
        assert_eq!(ev.decision, Decision::Ask);
        assert!(ev.reason.contains("outside the declared scope"));
        let anf = policy(EditPolicy::AutoNewFiles, Mode::Build, dir.path());
        assert_eq!(
            anf.evaluate("fs.propose_create", &other).decision,
            Decision::Allow
        );
        assert_eq!(
            anf.evaluate("fs.propose_edit", &other).decision,
            Decision::Ask
        );
    }

    #[test]
    fn locked_regions_deny() {
        let dir = tempfile::tempdir().unwrap();
        let file = "import x\n# analyst: lock\nSECRET = 1\n# analyst: unlock\ny = 2\n";
        std::fs::create_dir_all(dir.path().join("analysis")).unwrap();
        std::fs::write(dir.path().join("analysis/core.py"), file).unwrap();
        assert_eq!(locked_ranges(file), vec![(2, 4)]);
        let mut p = policy(EditPolicy::Auto, Mode::Build, dir.path());
        p.declare_scope(vec!["analysis/core.py".into()]);
        let touching =
            json!({"path":"analysis/core.py","edits":[{"old":"SECRET = 1","new":"SECRET = 2"}]});
        assert_eq!(
            p.evaluate("fs.propose_edit", &touching).decision,
            Decision::Deny
        );
        let outside = json!({"path":"analysis/core.py","edits":[{"old":"y = 2","new":"y = 3"}]});
        assert_eq!(
            p.evaluate("fs.propose_edit", &outside).decision,
            Decision::Allow
        );
        let full_keep = json!({"path":"analysis/core.py","content":"import z\n# analyst: lock\nSECRET = 1\n# analyst: unlock\n"});
        assert_eq!(
            p.evaluate("fs.propose_edit", &full_keep).decision,
            Decision::Allow
        );
        let full_break = json!({"path":"analysis/core.py","content":"import z\n"});
        assert_eq!(
            p.evaluate("fs.propose_edit", &full_break).decision,
            Decision::Deny
        );
        assert_eq!(
            p.evaluate("fs.propose_delete", &json!({"path":"analysis/core.py"}))
                .decision,
            Decision::Deny
        );
        assert_eq!(locked_ranges("# analyst: lock\na\nb\n"), vec![(1, 3)]);
    }

    /// G6: the second edit's `old` only exists after the first edit, so a check of each
    /// `old` against the original file never saw it.
    #[test]
    fn two_step_edit_cannot_rewrite_a_locked_region() {
        let dir = tempfile::tempdir().unwrap();
        let file = "x = 1\n# analyst: lock\nSECRET = 1\n# analyst: unlock\ny = 2\n";
        std::fs::write(dir.path().join("core.py"), file).unwrap();
        let mut p = policy(EditPolicy::Auto, Mode::Build, dir.path());
        p.declare_scope(vec!["core.py".into()]);
        let two_step = json!({"path":"core.py","edits":[
            {"old":"x = 1\n# analyst: lock\nSECRET","new":"x = 1\n# analyst: lock\nMARK"},
            {"old":"MARK = 1","new":"SECRET = 999"}
        ]});
        let ev = p.evaluate("fs.propose_edit", &two_step);
        assert_eq!(ev.decision, Decision::Deny, "{}", ev.reason);
        // Removing the markers, or moving the unlock marker, is also a change.
        let unlock = json!({"path":"core.py","edits":[{"old":"# analyst: lock\n","new":""}]});
        assert_eq!(
            p.evaluate("fs.propose_edit", &unlock).decision,
            Decision::Deny
        );
        // CRLF line endings do not defeat or trip the check.
        let crlf = file.replace('\n', "\r\n");
        std::fs::write(dir.path().join("core.py"), &crlf).unwrap();
        let outside = json!({"path":"core.py","edits":[{"old":"y = 2","new":"y = 3"}]});
        assert_eq!(
            p.evaluate("fs.propose_edit", &outside).decision,
            Decision::Allow
        );
        let inside = json!({"path":"core.py","edits":[{"old":"SECRET = 1","new":"SECRET = 2"}]});
        assert_eq!(
            p.evaluate("fs.propose_edit", &inside).decision,
            Decision::Deny
        );
    }

    /// G2: without a confining sandbox, run- and shell-class tools ask.
    #[test]
    fn unsandboxed_runs_ask() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = PolicyConfig {
            shell: AskPolicy::Auto,
            ..Default::default()
        };
        let mut p = Policy::new(dir.path(), cfg, Mode::Build).unwrap();
        for tool in ["run.script", "exec.snippet", "shell.run"] {
            let ev = p.evaluate(tool, &json!({}));
            assert_eq!(ev.decision, Decision::Ask, "{tool} before set_sandbox");
            assert!(ev.reason.contains("unsandboxed"), "{}", ev.reason);
        }
        for kind in ["limits", "none", "something-new"] {
            p.set_sandbox(kind);
            let ev = p.evaluate("exec.snippet", &json!({}));
            assert_eq!(ev.decision, Decision::Ask, "{kind}");
            assert!(ev.reason.contains("unsandboxed") && ev.reason.contains(kind));
            assert!(p.summary().contains("unsandboxed"));
        }
        // A denial is not softened into a question.
        assert_eq!(
            p.evaluate("run.script", &json!({"network": true})).decision,
            Decision::Deny
        );
        for kind in ["bwrap", "sandbox-exec"] {
            p.set_sandbox(kind);
            assert_eq!(
                p.evaluate("run.script", &json!({})).decision,
                Decision::Allow
            );
            assert_eq!(
                p.evaluate("shell.run", &json!({})).decision,
                Decision::Allow
            );
            assert!(!p.summary().contains("unsandboxed"));
        }
        p.set_sandbox("none");
        p.set_unsandboxed_runs(UnsandboxedRuns::Auto);
        assert_eq!(
            p.evaluate("run.script", &json!({})).decision,
            Decision::Allow
        );
        // Other classes are unaffected.
        assert_eq!(p.evaluate("pkg.add", &json!({})).decision, Decision::Ask);
        assert_eq!(p.evaluate("fs.read", &json!({})).decision, Decision::Allow);
    }

    /// G4: reserved directories are denied under every edit policy, in any case.
    #[test]
    fn reserved_paths_are_never_editable() {
        let dir = tempfile::tempdir().unwrap();
        for edits in [
            EditPolicy::Auto,
            EditPolicy::AutoNewFiles,
            EditPolicy::Review,
        ] {
            let mut p = policy(edits, Mode::Build, dir.path());
            p.declare_scope(vec![
                ".git/config".into(),
                ".analyst/project.toml".into(),
                ".GIT/hooks/pre-commit".into(),
            ]);
            for (tool, path) in [
                ("fs.propose_edit", ".git/config"),
                ("fs.propose_create", ".GIT/hooks/pre-commit"),
                ("fs.propose_create", ".git/hooks/post-checkout"),
                ("fs.propose_edit", ".analyst/project.toml"),
                ("fs.propose_delete", ".Analyst/project.toml"),
                ("fs.propose_create", "sub/.git/config"),
            ] {
                let ev = p.evaluate(tool, &json!({"path": path, "content": "x"}));
                assert_eq!(ev.decision, Decision::Deny, "{tool} {path} under {edits:?}");
            }
        }
    }

    /// G5: scope bookkeeping.
    #[test]
    fn scope_changes_are_classified_and_reset_per_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = policy(EditPolicy::Auto, Mode::Build, dir.path());
        assert_eq!(p.scope_change(&["a.py".into()]), ScopeChange::First);
        p.declare_scope(vec!["a.py".into(), "./b.py".into()]);
        assert_eq!(p.scope_change(&["b.py".into()]), ScopeChange::Narrows);
        assert_eq!(
            p.scope_change(&["a.py".into(), "c.py".into()]),
            ScopeChange::Widens(vec!["c.py".into()])
        );
        p.begin_turn();
        assert!(p.scope.is_none());
        assert_eq!(p.scope_change(&["z.py".into()]), ScopeChange::First);
        assert_eq!(
            p.evaluate("fs.propose_edit", &json!({"path":"a.py","content":"x"}))
                .decision,
            Decision::Ask
        );
    }

    #[cfg(unix)]
    #[test]
    fn edits_through_symlinks_are_denied() {
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(dir.path().join("data/x.csv"), "1").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out_link")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("data"), dir.path().join("alias")).unwrap();
        let mut p = policy(EditPolicy::Auto, Mode::Build, dir.path());
        p.declare_scope(vec!["out_link/evil.py".into(), "alias/x.csv".into()]);
        let ev = p.evaluate(
            "fs.propose_create",
            &json!({"path":"out_link/evil.py","content":"x"}),
        );
        assert_eq!(ev.decision, Decision::Deny);
        assert!(ev.reason.contains("outside the repository"));
        // `alias/x.csv` is really `data/x.csv`, which is protected.
        let ev = p.evaluate(
            "fs.propose_edit",
            &json!({"path":"alias/x.csv","content":"2"}),
        );
        assert_eq!(ev.decision, Decision::Deny, "{}", ev.reason);
    }
}
