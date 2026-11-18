//! The analysis side of the shared policy engine (`wb-policy`): maps `(tool, args)` onto the
//! engine's `Action`, the conversation mode onto its mode rules, and the project's
//! `[policy]` table onto its policy. All decisions are taken by `wb_policy::Session`.

use crate::tools::registry::{tool_class, ToolClass};
use analyst_protocol::config::{PolicyConfig, ProjectConfig, UnsandboxedRuns};
use analyst_protocol::evals::Mode;
use serde_json::Value;
use std::path::Path;
use wb_policy::config::analyst_class as class;
use wb_policy::{Action, ModeRules, PathRules, Session, CLASS_EDIT, CLASS_READ};

pub use wb_policy::ScopeChange;

/// Top-level directories no edit may touch (besides `.git/` at any depth).
pub const RESERVED_DIRS: &[&str] = &[".analyst"];

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

/// What each conversation mode offers (ARCHITECTURE 5.7). Modes are permission profiles
/// over the one loop: these rules decide which tools the model is offered and they are
/// enforced again when a tool is called, so nothing here depends on the prompt.
pub fn mode_rules(mode: Mode) -> ModeRules {
    use analyst_deck::tools::dispatch::{FLAG_NEW_CITATION, FLAG_NEW_ELEMENT};
    let name = mode.to_string();
    let read_only = || ModeRules::with_classes(mode.to_string(), [CLASS_READ, class::META]);
    match mode {
        Mode::Build => ModeRules::unrestricted(name),
        Mode::Plan => read_only(),
        Mode::Run => read_only()
            .allow_tool("run.script")
            .allow_tool("exec.snippet"),
        // The storyline only: nothing is rendered and no slide changes.
        Mode::Outline => read_only()
            .allow_tool("outline.propose")
            .allow_tool("export.run"),
        // Every deck tool; no code edits, script runs, packages, shell, or git.
        Mode::Slides => {
            let mut rules = read_only();
            for t in analyst_deck::tools::registry() {
                rules = rules.allow_tool(t.name);
            }
            rules
        }
        // Wording, layout choice, and order of existing slides: no new slides, no
        // deletions, no new claims (citations) or elements.
        Mode::Polish => read_only()
            .allow_tool("slide.propose_update")
            .allow_tool("slide.propose_move")
            .allow_tool("code.propose_edit")
            .allow_tool("deck.build")
            .allow_tool("skill.run_script")
            .allow_tool("export.run")
            .restrict(wb_policy::FLAG_CREATES_NEW)
            .restrict(FLAG_NEW_CITATION)
            .restrict(FLAG_NEW_ELEMENT),
    }
}

/// Whether turns in this mode can change files, so they start with a checkpoint.
pub fn mode_changes_files(mode: Mode) -> bool {
    !matches!(mode, Mode::Plan | Mode::Run)
}

pub(crate) fn class_name(c: ToolClass) -> &'static str {
    match c {
        ToolClass::Read => CLASS_READ,
        ToolClass::Meta => class::META,
        ToolClass::Edit => CLASS_EDIT,
        ToolClass::Run => class::RUN,
        ToolClass::Package => class::PACKAGE,
        ToolClass::Shell => class::SHELL,
        ToolClass::Git => class::GIT,
    }
}

/// The policy engine's view of a tool call; `None` for a tool that does not exist.
pub fn action_for(tool: &str, args: &Value) -> Option<Action> {
    let c = tool_class(tool)?;
    let mut a = Action::new(tool, class_name(c));
    if crate::tools::registry::is_deck_tool(tool) {
        // Deck builds, skill scripts, and exports run in the deck kernel's own sandbox, and
        // deck changes are evaluated with their real targets once they are prepared
        // ([`Policy::evaluate_action`]).
        return Some(a);
    }
    match c {
        ToolClass::Edit => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or("");
            a = a.targets([path]).creates_new(tool == "fs.propose_create");
        }
        ToolClass::Run => {
            // There is no network grant for runs, at any policy setting. A call that still
            // asks for one is refused outright so the model learns the rule.
            let asks = args
                .get("network")
                .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)));
            a = a.needs_sandbox(true).requests_network(asks);
        }
        ToolClass::Shell => a = a.needs_sandbox(true),
        _ => {}
    }
    Some(a)
}

/// Policy state for one conversation.
#[derive(Debug, Clone)]
pub struct Policy {
    pub config: PolicyConfig,
    mode: Mode,
    session: Session,
}

impl Policy {
    pub fn new(repo: &Path, config: PolicyConfig, mode: Mode) -> anyhow::Result<Policy> {
        Self::build(repo, config, UnsandboxedRuns::Ask, mode)
    }

    /// A policy from the whole project config (includes `policy.unsandboxed_runs`).
    pub fn for_project(repo: &Path, project: &ProjectConfig, mode: Mode) -> anyhow::Result<Policy> {
        Self::build(repo, project.policy.clone(), project.unsandboxed_runs, mode)
    }

    fn build(
        repo: &Path,
        config: PolicyConfig,
        unsandboxed: UnsandboxedRuns,
        mode: Mode,
    ) -> anyhow::Result<Policy> {
        let shared = config.to_shared(unsandboxed);
        let table = shared
            .to_policy()
            .map_err(|e| anyhow::anyhow!("protected glob: {e}"))?;
        let session = Session::new(table, mode_rules(mode))
            .with_paths(PathRules::new(repo, RESERVED_DIRS.iter().copied()))
            .with_unsandboxed(shared.unsandboxed_runs);
        Ok(Policy {
            config,
            mode,
            session,
        })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.session.mode = mode_rules(mode);
    }

    /// Record the sandbox kind the kernel reported in `kernel.hello`. Must be called before
    /// the first tool call; until then runs are treated as unsandboxed.
    pub fn set_sandbox(&mut self, kind: &str) {
        self.session.set_sandbox(kind);
    }

    pub fn set_unsandboxed_runs(&mut self, v: UnsandboxedRuns) {
        self.session.unsandboxed = match v {
            UnsandboxedRuns::Ask => wb_policy::Unsandboxed::Ask,
            UnsandboxedRuns::Auto => wb_policy::Unsandboxed::Auto,
        };
    }

    /// The sandbox kind last reported, if any.
    pub fn sandbox(&self) -> Option<&str> {
        self.session.sandbox()
    }

    /// Whether runs are confined by an OS sandbox.
    pub fn is_sandboxed(&self) -> bool {
        self.session.is_sandboxed()
    }

    /// Start of a turn: the declared scope does not carry over.
    pub fn begin_turn(&mut self) {
        self.session.begin_turn();
    }

    /// Files declared this turn (`declare_scope`); `None` = not declared.
    pub fn scope(&self) -> Option<&[String]> {
        self.session.scope()
    }

    /// Restore or clear the scope (rehydration from a transcript).
    pub fn set_scope(&mut self, files: Option<Vec<String>>) {
        self.session.set_scope(files);
    }

    /// Classify a declaration against the scope in force, without changing anything.
    pub fn scope_change(&self, files: &[String]) -> ScopeChange {
        self.session.scope_change(files)
    }

    /// Replace the scope. Callers gate widening through [`Policy::scope_change`] and the
    /// approver first; this method does not ask.
    pub fn declare_scope(&mut self, files: Vec<String>) {
        self.session.declare_scope(files);
    }

    pub fn is_protected(&self, path: &str) -> bool {
        self.session.policy.is_locked(path)
    }

    /// Add one target to the declared scope (a slide the agent announced with `new_slides`
    /// and has now created; a deck's single outline or build script).
    pub fn add_to_scope(&mut self, target: &str) {
        let mut scope = self.scope().map(<[String]>::to_vec).unwrap_or_default();
        if !scope.iter().any(|t| t == target) {
            scope.push(target.to_string());
        }
        self.session.declare_scope(scope);
    }

    /// Whether the policy applies no edit at all (the `read-only` preset).
    pub fn edits_denied(&self) -> bool {
        self.config.edits == analyst_protocol::config::EditPolicy::Deny
    }

    /// Evaluate an action built by the caller: a deck change with its real targets (the
    /// slide documents as repo-relative files) and the flags a mode may forbid.
    pub fn evaluate_action(&self, action: &Action) -> Evaluation {
        let ev = self.session.evaluate(action);
        Evaluation {
            decision: match ev.decision {
                wb_policy::Decision::Allow => Decision::Allow,
                wb_policy::Decision::Ask { .. } => Decision::Ask,
                wb_policy::Decision::Deny { .. } => Decision::Deny,
            },
            reason: ev.message,
        }
    }

    /// Whether a (normalised) path is in the scope declared this turn.
    pub fn in_scope(&self, path: &str) -> bool {
        self.session.in_scope(path)
    }

    /// Whether the mode makes a tool available at all.
    pub fn tool_available(&self, tool: &str) -> bool {
        tool_class(tool).is_some_and(|c| self.session.tool_available(tool, class_name(c)))
    }

    /// One-paragraph summary for the system prompt.
    pub fn summary(&self) -> String {
        let mut s = self.session.summary();
        if matches!(self.mode, Mode::Build | Mode::Slides | Mode::Polish)
            && self.scope().is_none_or(|f| f.is_empty())
            && self.config.edits != analyst_protocol::config::EditPolicy::Deny
        {
            s.push_str(
                " Call declare_scope once with every file and slide you intend to touch before changing anything.",
            );
        }
        s
    }

    /// Evaluate a tool call.
    pub fn evaluate(&self, tool: &str, args: &Value) -> Evaluation {
        let Some(action) = action_for(tool, args) else {
            return Evaluation {
                decision: Decision::Deny,
                reason: format!("unknown tool `{tool}`"),
            };
        };
        let ev = self.session.evaluate(&action);
        Evaluation {
            decision: match ev.decision {
                wb_policy::Decision::Allow => Decision::Allow,
                wb_policy::Decision::Ask { .. } => Decision::Ask,
                wb_policy::Decision::Deny { .. } => Decision::Deny,
            },
            reason: ev.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analyst_protocol::config::{EditPolicy, PolicyPreset};
    use serde_json::json;

    /// The tool mapping: every registered tool has an action, edits carry their path, runs
    /// and shell commands depend on the sandbox, and a `network` argument is refused. The
    /// rules themselves are tested in `wb-policy` (`session_tests`).
    #[test]
    fn tools_map_onto_the_shared_engine() {
        for spec in crate::tools::registry::registry() {
            assert!(action_for(spec.name, &json!({})).is_some(), "{}", spec.name);
        }
        assert!(action_for("nope", &json!({})).is_none());
        let a = action_for("fs.propose_create", &json!({"path": "a.py"})).unwrap();
        assert!(a.creates_new && a.targets == vec!["a.py"] && a.class == CLASS_EDIT);
        let a = action_for("run.script", &json!({"script": "s.py", "network": true})).unwrap();
        assert!(a.needs_sandbox && a.requests_network);
        assert!(
            !action_for("run.script", &json!({"network": false}))
                .unwrap()
                .requests_network
        );
        assert!(action_for("shell.run", &json!({})).unwrap().needs_sandbox);
        assert!(!action_for("pkg.add", &json!({})).unwrap().needs_sandbox);

        let dir = tempfile::tempdir().unwrap();
        let mut p = Policy::new(dir.path(), PolicyConfig::default(), Mode::Build).unwrap();
        p.set_sandbox("bwrap");
        assert_eq!(p.evaluate("nope", &json!({})).decision, Decision::Deny);
        let ev = p.evaluate("run.script", &json!({"script": "s.py", "network": "yes"}));
        assert_eq!(ev.decision, Decision::Deny);
        assert!(ev.reason.contains("never have network"));
        assert_eq!(
            p.evaluate("fs.propose_edit", &json!({"path": ".analyst/project.toml"}))
                .decision,
            Decision::Deny
        );
        // The tool schema offers no way to ask for network, and rejects unknown parameters.
        let spec = crate::tools::registry::registry()
            .into_iter()
            .find(|t| t.name == "run.script")
            .unwrap();
        assert!(spec.parameters["properties"].get("network").is_none());
        assert_eq!(spec.parameters["additionalProperties"], json!(false));
        // The kernel request has no such field either: one sent from elsewhere is dropped.
        let params: analyst_protocol::kernel::RunStartParams =
            serde_json::from_value(json!({"script": "s.py", "network": true})).unwrap();
        assert!(!serde_json::to_string(&params).unwrap().contains("network"));
        // Modes.
        p.set_mode(Mode::Plan);
        assert!(!p.tool_available("run.script") && p.tool_available("fs.read"));
        p.set_mode(Mode::Run);
        assert!(p.tool_available("exec.snippet") && !p.tool_available("fs.propose_edit"));
    }

    /// The read-only preset through the analysis tool names: no edit is applied or even
    /// offered for review, runs of existing scripts start, everything else is denied.
    #[test]
    fn read_only_preset_denies_edits() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = PolicyConfig::default();
        cfg.apply_preset(PolicyPreset::ReadOnly);
        assert_eq!(cfg.edits, EditPolicy::Deny);
        let mut p = Policy::new(dir.path(), cfg, Mode::Build).unwrap();
        p.set_sandbox("bwrap");
        p.declare_scope(vec!["a.py".into()]);
        for tool in ["fs.propose_edit", "fs.propose_create", "fs.propose_delete"] {
            let ev = p.evaluate(tool, &json!({"path": "a.py", "content": "x"}));
            assert_eq!(ev.decision, Decision::Deny, "{tool}");
            assert!(ev.reason.contains("read-only"), "{}", ev.reason);
        }
        for tool in ["pkg.add", "shell.run", "git.commit"] {
            assert_eq!(
                p.evaluate(tool, &json!({})).decision,
                Decision::Deny,
                "{tool}"
            );
        }
        assert_eq!(
            p.evaluate("run.script", &json!({"script": "scripts/a.py"}))
                .decision,
            Decision::Allow
        );
        assert_eq!(
            p.evaluate("fs.read", &json!({"path": "a.py"})).decision,
            Decision::Allow
        );
        assert!(!p.summary().contains("declare_scope"));
    }
}
