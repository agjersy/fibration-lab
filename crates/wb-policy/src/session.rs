// Ported from the stateful half of Personal Analyst `crates/core/src/policy.rs` as it stood
// after the security review (REVIEW G2, G4, G5, G7), so that both workbenches get it.
//! Policy state for one conversation: the policy table, the conversation mode, the scope
//! declared this turn, the sandbox the kernel reported, and (for apps whose edit targets are
//! files) the path rules.
//!
//! Evaluation order (first match wins):
//!
//! 1. The mode does not offer the action: `Deny(Mode)`.
//! 2. The action asks for network access: `Deny(Network)`. No setting grants it.
//! 3. An edit names a path that is not inside the root (lexically or through a symlink):
//!    `Deny(Path)`; a path under a reserved directory, or one that is or resolves to a
//!    locked path: `Deny(Locked)`.
//! 4. The policy table and the edit gate ([`Policy::explain`]).
//! 5. An action that only an OS sandbox confines (`needs_sandbox`), when no confining
//!    sandbox was reported and the project did not opt out: `Ask(Unsandboxed)`. A denial
//!    stays a denial.

use crate::paths::{fs_case_insensitive, PathDenied, PathRules};
use crate::{
    normalize_rel_path, Action, Decision, Evaluation, ModeRules, Policy, Reason, Scope, CLASS_EDIT,
};
use serde::{Deserialize, Serialize};

/// What sandbox-dependent actions do when nothing confines them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Unsandboxed {
    /// Ask first, whatever the class level says.
    #[default]
    Ask,
    /// Follow the class level.
    Auto,
}

/// What a scope declaration would do to the scope in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeChange {
    /// No scope yet this turn: free.
    First,
    /// Same targets or fewer: free.
    Narrows,
    /// Adds these targets to an existing scope: needs approval.
    Widens(Vec<String>),
}

/// Sandbox kinds that actually confine a run. Everything else (`limits`, `none`, unknown,
/// not yet reported) leaves the filesystem and network open.
pub fn sandbox_confines(kind: &str) -> bool {
    matches!(kind, "bwrap" | "sandbox-exec" | "landlock")
}

#[derive(Debug, Clone)]
pub struct Session {
    pub policy: Policy,
    pub mode: ModeRules,
    /// Targets the agent declared it intends to change, in declaration order. `None` = not
    /// declared. Reset at the start of every turn ([`Session::begin_turn`]).
    scope: Option<Vec<String>>,
    /// The declaration covers items that do not exist yet.
    scope_allows_new: bool,
    /// Sandbox kind the kernel reported; `None` until [`Session::set_sandbox`] is called,
    /// which is treated as unsandboxed.
    sandbox: Option<String>,
    pub unsandboxed: Unsandboxed,
    paths: Option<PathRules>,
}

impl Session {
    pub fn new(policy: Policy, mode: ModeRules) -> Self {
        Session {
            policy,
            mode,
            scope: None,
            scope_allows_new: false,
            sandbox: None,
            unsandboxed: Unsandboxed::Ask,
            paths: None,
        }
    }

    /// Edit targets are files under `rules.root()`. Locked globs match in the letter case
    /// the volume uses: on a case-insensitive volume `Data/X.csv` is the locked `data/x.csv`.
    pub fn with_paths(mut self, rules: PathRules) -> Self {
        self.policy
            .locked
            .set_case_insensitive(fs_case_insensitive(rules.root()));
        self.paths = Some(rules);
        self
    }

    pub fn with_unsandboxed(mut self, v: Unsandboxed) -> Self {
        self.unsandboxed = v;
        self
    }

    /// Replace the policy table, keeping the scope, sandbox, and path rules.
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
        if let Some(rules) = &self.paths {
            self.policy
                .locked
                .set_case_insensitive(fs_case_insensitive(rules.root()));
        }
    }

    /// Record the sandbox kind the kernel reported. Until this is called, sandbox-dependent
    /// actions are treated as unsandboxed.
    pub fn set_sandbox(&mut self, kind: &str) {
        self.sandbox = Some(kind.to_string());
    }

    pub fn sandbox(&self) -> Option<&str> {
        self.sandbox.as_deref()
    }

    /// Whether an OS sandbox confines runs.
    pub fn is_sandboxed(&self) -> bool {
        self.sandbox
            .as_deref()
            .map(sandbox_confines)
            .unwrap_or(false)
    }

    /// Start of a turn: the declared scope does not carry over.
    pub fn begin_turn(&mut self) {
        self.scope = None;
        self.scope_allows_new = false;
    }

    fn normalize_scope(&self, targets: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in targets {
            let n = if self.paths.is_some() {
                match normalize_rel_path(t) {
                    Ok(n) => n,
                    Err(_) => continue,
                }
            } else {
                t.clone()
            };
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }

    /// The scope declared this turn.
    pub fn scope(&self) -> Option<&[String]> {
        self.scope.as_deref()
    }

    /// Classify a declaration against the scope in force, without changing anything.
    pub fn scope_change(&self, targets: &[String]) -> ScopeChange {
        let new = self.normalize_scope(targets);
        match &self.scope {
            None => ScopeChange::First,
            Some(cur) => {
                let added: Vec<String> = new.into_iter().filter(|t| !cur.contains(t)).collect();
                if added.is_empty() {
                    ScopeChange::Narrows
                } else {
                    ScopeChange::Widens(added)
                }
            }
        }
    }

    /// Replace the scope. Callers gate widening through [`Session::scope_change`] and the
    /// user's approval first; this method does not ask.
    pub fn declare_scope(&mut self, targets: Vec<String>) {
        self.scope = Some(self.normalize_scope(&targets));
    }

    /// Restore a scope (for example from a transcript), or clear it.
    pub fn set_scope(&mut self, targets: Option<Vec<String>>) {
        self.scope = targets.map(|t| self.normalize_scope(&t));
    }

    pub fn set_scope_allows_new(&mut self, yes: bool) {
        self.scope_allows_new = yes;
    }

    /// Whether a (normalised) target is in the scope declared this turn.
    pub fn in_scope(&self, target: &str) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|s| s.iter().any(|t| t == target))
    }

    fn scope_for_policy(&self) -> Scope {
        match &self.scope {
            None => Scope::undeclared(),
            Some(t) => Scope::declared(t.iter().cloned()).with_new(self.scope_allows_new),
        }
    }

    /// Whether the mode offers a tool at all.
    pub fn tool_available(&self, tool: &str, class: &str) -> bool {
        self.mode.tool_available(tool, class)
    }

    pub fn evaluate(&self, action: &Action) -> Evaluation {
        if let Some(denied) = self.mode.check(action) {
            return denied;
        }
        if action.requests_network {
            return Evaluation::deny(
                Reason::Network,
                "runs never have network access; there is no `network` parameter. Read data from the repository or a configured data directory",
            );
        }
        let mut action = action.clone();
        if action.class == CLASS_EDIT {
            if let Some(rules) = &self.paths {
                let mut targets = Vec::with_capacity(action.targets.len());
                for raw in &action.targets {
                    let names = match rules.check_edit(raw) {
                        Ok(n) => n,
                        Err(PathDenied::Outside(m)) => return Evaluation::deny(Reason::Path, m),
                        Err(PathDenied::Reserved(m)) => return Evaluation::deny(Reason::Locked, m),
                    };
                    let given = names[0].clone();
                    if self.policy.is_locked(&given) {
                        return Evaluation::deny(
                            Reason::Locked,
                            format!("`{given}` matches a protected pattern"),
                        );
                    }
                    // The link target may be protected even when the given name is not.
                    if let Some(real) = names[1..].iter().find(|n| self.policy.is_locked(n)) {
                        return Evaluation::deny(
                            Reason::Locked,
                            format!("`{given}` resolves to `{real}`, which is protected"),
                        );
                    }
                    targets.push(given);
                }
                action.targets = targets;
            }
        }
        let base = self.policy.explain(&action, &self.scope_for_policy());
        self.unsandboxed_gate(&action, base)
    }

    /// Sandbox-dependent actions ask when nothing confines them (G2), unless the project
    /// opted out. A denial stays a denial.
    fn unsandboxed_gate(&self, action: &Action, base: Evaluation) -> Evaluation {
        if !action.needs_sandbox
            || base.decision.is_deny()
            || self.is_sandboxed()
            || self.unsandboxed == Unsandboxed::Auto
        {
            return base;
        }
        let kind = self.sandbox.as_deref().unwrap_or("unknown");
        Evaluation::ask(
            Reason::Unsandboxed,
            format!(
                "`{}` would run unsandboxed (kernel sandbox: {kind}): it can read and write anything your account can, and network limits are not enforced",
                action.tool
            ),
        )
    }

    pub fn decide(&self, action: &Action) -> Decision {
        self.evaluate(action).decision
    }

    /// One-paragraph summary for the system prompt.
    pub fn summary(&self) -> String {
        let mut s = self
            .policy
            .summary(Some(&self.mode), &self.scope_for_policy());
        s.push_str(" Network for runs: never (no setting grants it).");
        if let Some(rules) = &self.paths {
            s.push_str(&format!(" {}", rules.reserved_summary()));
        }
        let kind = self.sandbox.as_deref().unwrap_or("unknown");
        if self.is_sandboxed() {
            s.push_str(&format!(" Sandbox: {kind} (runs are confined)."));
        } else {
            match self.unsandboxed {
                Unsandboxed::Ask => s.push_str(&format!(
                    " Sandbox: {kind}: runs are unsandboxed, so every run, snippet, and shell command asks the user first regardless of the runs policy."
                )),
                Unsandboxed::Auto => s.push_str(&format!(
                    " Sandbox: {kind}: runs are unsandboxed (the project allows this without asking)."
                )),
            }
        }
        s.push_str(
            " The declared scope lasts for this turn only; adding to it later needs the user's approval.",
        );
        s
    }
}
