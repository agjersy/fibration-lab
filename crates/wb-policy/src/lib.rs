// Ported from Personal Analyst `crates/core/src/policy.rs` at commit f229e4d.
// Adapted: the vocabulary is generic (string policy classes, an `Action` built by the app
// instead of `(tool, args)`, mode rules as data, locked ids or globs instead of protected
// path globs only) so that Personal Present and Personal Analyst can both map their tools
// onto it. No file I/O happens here; the app passes what an action changes.
//! Shared-to-be crate `wb-policy` (BUILD-SPEC section 4). Must not depend on Present-specific crates.
//!
//! Policy engine: `(Action, Scope) -> Allow | Ask | Deny` from a policy table, an edit gate,
//! locked items, the declared edit scope, and optional mode rules.
//!
//! Evaluation order (first match wins):
//!
//! 1. Mode rules, when given: a tool or class the mode does not offer, or an action carrying
//!    a restricted flag, is `Deny(Mode)`.
//! 2. A tool in `denied_tools` is `Deny(Policy)`.
//! 3. Class `read` is `Allow`.
//! 4. A class (or MCP server) at level `deny` is `Deny(Policy)`.
//! 5. An action with a locked target is `Deny(Locked)`.
//! 6. An action marked `always_ask` is `Ask(AlwaysAsk)`.
//! 7. Class `edit` follows the edit gate: `review` asks; `auto-new` allows new items and asks
//!    for the rest; `auto` allows what the declared scope covers and asks (`OutOfScope`) for
//!    the rest.
//! 8. Any other class follows its level: `auto` allows, `ask` asks. A class with no level
//!    asks.

pub mod config;
pub mod conflict;
pub mod regions;

pub use config::{AnalystPolicyConfig, McpConfig, PresentPolicyConfig};
pub use conflict::{check_conflict, content_hash, Conflict};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

/// Policy class of read-only tools: always allowed.
pub const CLASS_READ: &str = "read";
/// Policy class of tools that change the work product: follows the edit gate.
pub const CLASS_EDIT: &str = "edit";
/// Policy class of MCP tools: follows the per-server level, then the `mcp` class level.
pub const CLASS_MCP: &str = "mcp";
/// Restriction flag that every action with `creates_new` carries implicitly.
pub const FLAG_CREATES_NEW: &str = "creates_new";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("lock pattern `{pattern}`: {message}")]
    BadPattern { pattern: String, message: String },
}

/// Why a decision was taken.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The policy table or the edit gate says so.
    Policy,
    /// The edit is outside the declared scope (or no scope was declared).
    OutOfScope,
    /// A target is locked.
    Locked,
    /// The tool always asks, whatever the policy.
    AlwaysAsk,
    /// The conversation mode does not offer this action.
    Mode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Ask { reason: Reason },
    Deny { reason: Reason },
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow)
    }
    pub fn is_ask(&self) -> bool {
        matches!(self, Decision::Ask { .. })
    }
    pub fn is_deny(&self) -> bool {
        matches!(self, Decision::Deny { .. })
    }
    pub fn reason(&self) -> Option<Reason> {
        match self {
            Decision::Allow => None,
            Decision::Ask { reason } | Decision::Deny { reason } => Some(*reason),
        }
    }
}

/// A decision with a sentence that can be shown to the user and returned to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub decision: Decision,
    pub message: String,
}

impl Evaluation {
    fn allow(message: impl Into<String>) -> Self {
        Evaluation {
            decision: Decision::Allow,
            message: message.into(),
        }
    }
    fn ask(reason: Reason, message: impl Into<String>) -> Self {
        Evaluation {
            decision: Decision::Ask { reason },
            message: message.into(),
        }
    }
    fn deny(reason: Reason, message: impl Into<String>) -> Self {
        Evaluation {
            decision: Decision::Deny { reason },
            message: message.into(),
        }
    }
}

/// Level of a policy class or an MCP server.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    #[default]
    Ask,
    Auto,
    Deny,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ask => "ask",
            Level::Auto => "auto",
            Level::Deny => "deny",
        }
    }
}

/// Edit gating mode. Accepts the app spellings `auto-new-slides` (Present) and
/// `auto-new-files` (Analyst) for `auto-new`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum EditGate {
    /// Every edit pauses for review.
    #[default]
    #[serde(rename = "review")]
    Review,
    /// New items apply immediately; changes to existing items pause.
    #[serde(
        rename = "auto-new",
        alias = "auto-new-slides",
        alias = "auto-new-files"
    )]
    AutoNew,
    /// In-scope edits apply immediately; out-of-scope edits pause.
    #[serde(rename = "auto")]
    Auto,
}

/// What a tool call is about to do, as the app describes it to the policy engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Action {
    /// Internal tool name (`slide.propose_update`, `mcp.<server>.<tool>`).
    pub tool: String,
    /// Policy class (`read`, `edit`, `code`, `skills_scripts`, `mcp`, `export`, `shell`,
    /// `package`, `git`, `run`, ...).
    pub class: String,
    /// Ids or paths the action changes.
    #[serde(default)]
    pub targets: Vec<String>,
    /// The action creates an item that does not exist yet.
    #[serde(default)]
    pub creates_new: bool,
    /// The tool asks whatever the policy says.
    #[serde(default)]
    pub always_ask: bool,
    /// App-defined facts about the action that a mode can restrict (for example
    /// `new-citation`). `creates_new` counts as the flag `creates_new`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
}

impl Action {
    pub fn new(tool: impl Into<String>, class: impl Into<String>) -> Self {
        Action {
            tool: tool.into(),
            class: class.into(),
            ..Default::default()
        }
    }
    pub fn read(tool: impl Into<String>) -> Self {
        Action::new(tool, CLASS_READ)
    }
    pub fn edit<I, S>(tool: impl Into<String>, targets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Action::new(tool, CLASS_EDIT).targets(targets)
    }
    pub fn targets<I, S>(mut self, targets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.targets = targets.into_iter().map(Into::into).collect();
        self
    }
    pub fn creates_new(mut self, yes: bool) -> Self {
        self.creates_new = yes;
        self
    }
    pub fn always_ask(mut self, yes: bool) -> Self {
        self.always_ask = yes;
        self
    }
    pub fn flag(mut self, flag: impl Into<String>) -> Self {
        self.flags.push(flag.into());
        self
    }

    /// Server of an `mcp.<server>.<tool>` tool name.
    pub fn mcp_server(&self) -> Option<&str> {
        let rest = self.tool.strip_prefix("mcp.")?;
        let server = rest.split('.').next()?;
        (!server.is_empty()).then_some(server)
    }

    fn has_flag(&self, flag: &str) -> bool {
        (flag == FLAG_CREATES_NEW && self.creates_new) || self.flags.iter().any(|f| f == flag)
    }
}

/// The edit scope the agent declared for the current task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Scope {
    /// Ids or paths the agent said it will change. `None` = nothing declared yet.
    #[serde(default)]
    pub declared: Option<BTreeSet<String>>,
    /// The declaration says new items will be created (their ids need not be known yet).
    #[serde(default)]
    pub allow_new: bool,
}

impl Scope {
    /// No scope declared.
    pub fn undeclared() -> Self {
        Scope::default()
    }
    pub fn declared<I, S>(targets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Scope {
            declared: Some(targets.into_iter().map(Into::into).collect()),
            allow_new: false,
        }
    }
    pub fn with_new(mut self, allow_new: bool) -> Self {
        self.allow_new = allow_new;
        self
    }
    /// Replace the declaration.
    pub fn declare<I, S>(&mut self, targets: I, allow_new: bool)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.declared = Some(targets.into_iter().map(Into::into).collect());
        self.allow_new = allow_new;
    }
    /// Add one target to a declared scope (for example the id a created item received).
    pub fn add(&mut self, target: impl Into<String>) {
        self.declared
            .get_or_insert_with(BTreeSet::new)
            .insert(target.into());
    }
    pub fn clear(&mut self) {
        *self = Scope::default();
    }
    pub fn is_declared(&self) -> bool {
        self.declared.is_some()
    }
    pub fn contains(&self, target: &str) -> bool {
        self.declared.as_ref().is_some_and(|s| s.contains(target))
    }
}

/// Locked items: exact ids and glob patterns. An entry matches a target when it is equal to
/// it or, read as a glob (`*`, `?`, `**`, `[..]`, `{a,b}`), matches it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "Vec<String>", into = "Vec<String>")]
pub struct LockSet {
    entries: BTreeSet<String>,
    globs: GlobSet,
}

impl PartialEq for LockSet {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}
impl Eq for LockSet {}

impl From<Vec<String>> for LockSet {
    fn from(v: Vec<String>) -> Self {
        LockSet::new(v)
    }
}
impl From<LockSet> for Vec<String> {
    fn from(l: LockSet) -> Self {
        l.entries.into_iter().collect()
    }
}

impl LockSet {
    /// Lenient: an entry that is not a valid glob still matches as an exact id.
    pub fn new<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut l = LockSet {
            entries: entries.into_iter().map(Into::into).collect(),
            globs: GlobSet::empty(),
        };
        l.rebuild();
        l
    }

    /// Strict: every entry must be a valid glob (Analyst `protected` semantics).
    pub fn strict<I, S>(entries: I) -> Result<Self, PolicyError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let entries: BTreeSet<String> = entries.into_iter().map(Into::into).collect();
        for e in &entries {
            Glob::new(e).map_err(|err| PolicyError::BadPattern {
                pattern: e.clone(),
                message: err.kind().to_string(),
            })?;
        }
        Ok(LockSet::new(entries))
    }

    fn rebuild(&mut self) {
        let mut b = GlobSetBuilder::new();
        for e in &self.entries {
            if let Ok(g) = Glob::new(e) {
                b.add(g);
            }
        }
        self.globs = b.build().unwrap_or_else(|_| GlobSet::empty());
    }

    pub fn insert(&mut self, entry: impl Into<String>) -> bool {
        let added = self.entries.insert(entry.into());
        if added {
            self.rebuild();
        }
        added
    }

    pub fn remove(&mut self, entry: &str) -> bool {
        let removed = self.entries.remove(entry);
        if removed {
            self.rebuild();
        }
        removed
    }

    pub fn is_locked(&self, target: &str) -> bool {
        self.entries.contains(target) || self.globs.is_match(target)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }
}

/// What a conversation mode offers, as data. Apps define their own modes (plan / build /
/// run, outline / build / polish) and pass the active one to `Policy::evaluate_in_mode`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ModeRules {
    /// Name used in messages (`polish`).
    pub name: String,
    /// Policy classes the mode offers. `*` offers every class.
    pub allowed_classes: BTreeSet<String>,
    /// Tools offered although their class is not (Analyst run mode: `run.script`).
    pub allowed_tools: BTreeSet<String>,
    /// Tools the mode never offers, whatever their class.
    pub denied_tools: BTreeSet<String>,
    /// Action flags the mode forbids (`creates_new`, or app-defined flags such as
    /// `new-citation`). An action carrying one is denied.
    pub restrictions: Vec<String>,
}

impl ModeRules {
    /// A mode that offers everything.
    pub fn unrestricted(name: impl Into<String>) -> Self {
        ModeRules {
            name: name.into(),
            allowed_classes: BTreeSet::from(["*".to_string()]),
            ..Default::default()
        }
    }

    /// A mode that offers the given classes only.
    pub fn with_classes<I, S>(name: impl Into<String>, classes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ModeRules {
            name: name.into(),
            allowed_classes: classes.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    }

    pub fn allow_tool(mut self, tool: impl Into<String>) -> Self {
        self.allowed_tools.insert(tool.into());
        self
    }
    pub fn deny_tool(mut self, tool: impl Into<String>) -> Self {
        self.denied_tools.insert(tool.into());
        self
    }
    pub fn restrict(mut self, flag: impl Into<String>) -> Self {
        self.restrictions.push(flag.into());
        self
    }

    /// Whether the mode offers a tool at all (used to build the tool list for the model).
    pub fn tool_available(&self, tool: &str, class: &str) -> bool {
        if self.denied_tools.contains(tool) {
            return false;
        }
        self.allowed_tools.contains(tool)
            || self.allowed_classes.contains("*")
            || self.allowed_classes.contains(class)
    }

    /// `Some(denial)` when the mode forbids the action, `None` when the policy decides.
    pub fn check(&self, action: &Action) -> Option<Evaluation> {
        let mode = if self.name.is_empty() {
            "this"
        } else {
            self.name.as_str()
        };
        if !self.tool_available(&action.tool, &action.class) {
            return Some(Evaluation::deny(
                Reason::Mode,
                format!("tool `{}` is not available in {mode} mode", action.tool),
            ));
        }
        self.restrictions
            .iter()
            .find(|r| action.has_flag(r))
            .map(|r| {
                Evaluation::deny(
                    Reason::Mode,
                    format!("{mode} mode does not allow `{r}` (tool `{}`)", action.tool),
                )
            })
    }
}

/// Policy state: the base table from the project file, optionally with session overrides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Policy {
    pub edits: EditGate,
    pub max_steps: u32,
    /// Policy class -> level. A class that is not listed asks; `read` is always allowed.
    pub classes: BTreeMap<String, Level>,
    /// Locked items: never changeable by the agent.
    pub locked: LockSet,
    /// MCP server -> level; servers not listed use the `mcp` class level.
    pub mcp: BTreeMap<String, Level>,
    /// Tools switched off by configuration.
    pub denied_tools: BTreeSet<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            edits: EditGate::Review,
            max_steps: 40,
            classes: BTreeMap::new(),
            locked: LockSet::default(),
            mcp: BTreeMap::new(),
            denied_tools: BTreeSet::new(),
        }
    }
}

/// Session overrides layered over a base policy. Absent fields keep the base value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct PolicyPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edits: Option<EditGate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    /// Class levels to set.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub classes: BTreeMap<String, Level>,
    /// MCP server levels to set.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, Level>,
    /// Replace the whole locked set (applied before `lock` and `unlock`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<Vec<String>>,
    /// Entries to add to the locked set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lock: Vec<String>,
    /// Entries to remove from the locked set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unlock: Vec<String>,
    /// Tools to switch off.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deny_tools: Vec<String>,
    /// Tools to switch back on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_tools: Vec<String>,
}

impl PolicyPatch {
    pub fn is_empty(&self) -> bool {
        *self == PolicyPatch::default()
    }

    /// Combine two patches; `later` wins where both set a value.
    pub fn merged(&self, later: &PolicyPatch) -> PolicyPatch {
        let mut out = self.clone();
        if later.edits.is_some() {
            out.edits = later.edits;
        }
        if later.max_steps.is_some() {
            out.max_steps = later.max_steps;
        }
        out.classes
            .extend(later.classes.iter().map(|(k, v)| (k.clone(), *v)));
        out.mcp
            .extend(later.mcp.iter().map(|(k, v)| (k.clone(), *v)));
        if later.locked.is_some() {
            out.locked = later.locked.clone();
            out.lock.clear();
            out.unlock.clear();
        }
        for l in &later.lock {
            out.unlock.retain(|u| u != l);
            if !out.lock.contains(l) {
                out.lock.push(l.clone());
            }
        }
        for u in &later.unlock {
            out.lock.retain(|l| l != u);
            if !out.unlock.contains(u) {
                out.unlock.push(u.clone());
            }
        }
        for t in &later.deny_tools {
            out.allow_tools.retain(|a| a != t);
            if !out.deny_tools.contains(t) {
                out.deny_tools.push(t.clone());
            }
        }
        for t in &later.allow_tools {
            out.deny_tools.retain(|d| d != t);
            if !out.allow_tools.contains(t) {
                out.allow_tools.push(t.clone());
            }
        }
        out
    }
}

impl Policy {
    pub fn new() -> Self {
        Policy::default()
    }

    pub fn with_edits(mut self, edits: EditGate) -> Self {
        self.edits = edits;
        self
    }
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }
    pub fn with_class(mut self, class: impl Into<String>, level: Level) -> Self {
        self.classes.insert(class.into(), level);
        self
    }
    pub fn with_mcp_server(mut self, server: impl Into<String>, level: Level) -> Self {
        self.mcp.insert(server.into(), level);
        self
    }
    pub fn with_locked<I, S>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.locked = LockSet::new(entries);
        self
    }

    /// The policy with session overrides applied. The base is not changed.
    pub fn with_overrides(&self, patch: &PolicyPatch) -> Policy {
        let mut p = self.clone();
        if let Some(e) = patch.edits {
            p.edits = e;
        }
        if let Some(m) = patch.max_steps {
            p.max_steps = m;
        }
        for (k, v) in &patch.classes {
            p.classes.insert(k.clone(), *v);
        }
        for (k, v) in &patch.mcp {
            p.mcp.insert(k.clone(), *v);
        }
        if let Some(l) = &patch.locked {
            p.locked = LockSet::new(l.iter().cloned());
        }
        for l in &patch.lock {
            p.locked.insert(l.clone());
        }
        for u in &patch.unlock {
            p.locked.remove(u);
        }
        for t in &patch.deny_tools {
            p.denied_tools.insert(t.clone());
        }
        for t in &patch.allow_tools {
            p.denied_tools.remove(t);
        }
        p
    }

    /// Level of a class: `read` is auto, a listed class has its level, anything else asks.
    pub fn level(&self, class: &str) -> Level {
        match self.classes.get(class) {
            Some(l) => *l,
            None if class == CLASS_READ => Level::Auto,
            None => Level::Ask,
        }
    }

    /// Level that applies to an action: for class `mcp` the server's level when one is set.
    pub fn level_for(&self, action: &Action) -> Level {
        if action.class == CLASS_MCP {
            if let Some(l) = action.mcp_server().and_then(|s| self.mcp.get(s)) {
                return *l;
            }
        }
        self.level(&action.class)
    }

    pub fn is_locked(&self, target: &str) -> bool {
        self.locked.is_locked(target)
    }

    /// Evaluate an action against the policy and the declared scope.
    pub fn evaluate(&self, action: &Action, scope: &Scope) -> Decision {
        self.explain(action, scope).decision
    }

    /// Evaluate with the mode rules applied first.
    pub fn evaluate_in_mode(&self, mode: &ModeRules, action: &Action, scope: &Scope) -> Decision {
        self.explain_in_mode(mode, action, scope).decision
    }

    /// `evaluate_in_mode` with the explanation.
    pub fn explain_in_mode(&self, mode: &ModeRules, action: &Action, scope: &Scope) -> Evaluation {
        match mode.check(action) {
            Some(denied) => denied,
            None => self.explain(action, scope),
        }
    }

    /// `evaluate` with the explanation.
    pub fn explain(&self, action: &Action, scope: &Scope) -> Evaluation {
        let tool = &action.tool;
        let class = action.class.as_str();
        if self.denied_tools.contains(tool) {
            return Evaluation::deny(
                Reason::Policy,
                format!("tool `{tool}` is switched off by policy"),
            );
        }
        if class == CLASS_READ {
            return Evaluation::allow("read-only");
        }
        let level = self.level_for(action);
        if level == Level::Deny {
            let what = match action.mcp_server() {
                Some(s) if class == CLASS_MCP && self.mcp.contains_key(s) => {
                    format!("MCP server `{s}`")
                }
                _ => format!("{class} policy"),
            };
            return Evaluation::deny(Reason::Policy, format!("{what} is deny"));
        }
        if let Some(t) = action.targets.iter().find(|t| self.is_locked(t)) {
            return Evaluation::deny(Reason::Locked, format!("`{t}` is locked"));
        }
        if action.always_ask {
            return Evaluation::ask(
                Reason::AlwaysAsk,
                format!("tool `{tool}` always requires approval"),
            );
        }
        if class == CLASS_EDIT {
            return self.explain_edit(action, scope);
        }
        match level {
            Level::Auto => Evaluation::allow(format!("{class} policy is auto")),
            _ => Evaluation::ask(Reason::Policy, format!("{class} policy is ask")),
        }
    }

    fn explain_edit(&self, action: &Action, scope: &Scope) -> Evaluation {
        match self.edits {
            EditGate::Review => Evaluation::ask(Reason::Policy, "edits policy is review"),
            EditGate::AutoNew if action.creates_new => Evaluation::allow("new item under auto-new"),
            EditGate::AutoNew => Evaluation::ask(Reason::Policy, "existing item under auto-new"),
            EditGate::Auto => {
                if !scope.is_declared() {
                    return Evaluation::ask(
                        Reason::OutOfScope,
                        "no scope declared; edit requires approval",
                    );
                }
                if action.creates_new && scope.allow_new {
                    return Evaluation::allow("new item declared in scope under auto");
                }
                if action.targets.is_empty() {
                    return Evaluation::ask(
                        Reason::OutOfScope,
                        if action.creates_new {
                            "the declared scope does not include new items"
                        } else {
                            "the edit names no target in the declared scope"
                        },
                    );
                }
                match action.targets.iter().find(|t| !scope.contains(t)) {
                    None => Evaluation::allow("in declared scope under auto"),
                    Some(t) => Evaluation::ask(
                        Reason::OutOfScope,
                        format!("`{t}` is outside the declared scope"),
                    ),
                }
            }
        }
    }

    /// One-paragraph summary for the system prompt, so the model knows what needs approval.
    pub fn summary(&self, mode: Option<&ModeRules>, scope: &Scope) -> String {
        let mut s = String::new();
        if let Some(m) = mode.filter(|m| !m.name.is_empty()) {
            s.push_str(&format!("Mode: {}. ", m.name));
        }
        let edits = match self.edits {
            EditGate::Review => "every edit pauses for review",
            EditGate::AutoNew => {
                "new items are written immediately, edits to existing items pause for review"
            }
            EditGate::Auto => "in-scope edits apply immediately; out-of-scope edits pause",
        };
        s.push_str(&format!("Edits: {edits}."));
        for (class, level) in &self.classes {
            if class != CLASS_READ && class != CLASS_EDIT {
                s.push_str(&format!(" {class}: {}.", level.as_str()));
            }
        }
        for (server, level) in &self.mcp {
            s.push_str(&format!(" mcp.{server}: {}.", level.as_str()));
        }
        s.push_str(&format!(" Max steps per turn: {}.", self.max_steps));
        if !self.denied_tools.is_empty() {
            let tools: Vec<&str> = self.denied_tools.iter().map(String::as_str).collect();
            s.push_str(&format!(" Switched off: {}.", tools.join(", ")));
        }
        if !self.locked.is_empty() {
            let locked: Vec<&str> = self.locked.iter().collect();
            s.push_str(&format!(" Locked (never editable): {}.", locked.join(", ")));
        }
        if let Some(m) = mode.filter(|m| !m.restrictions.is_empty()) {
            s.push_str(&format!(
                " Not allowed in this mode: {}.",
                m.restrictions.join(", ")
            ));
        }
        let edits_offered = mode.is_none_or(|m| {
            m.allowed_classes.contains("*") || m.allowed_classes.contains(CLASS_EDIT)
        });
        match &scope.declared {
            Some(t) if !t.is_empty() || scope.allow_new => {
                let mut items: Vec<&str> = t.iter().map(String::as_str).collect();
                if scope.allow_new {
                    items.push("new items");
                }
                s.push_str(&format!(" Declared scope: {}.", items.join(", ")));
            }
            _ if edits_offered => s.push_str(
                " No scope declared yet: declare the scope before editing, or every edit will need approval.",
            ),
            _ => {}
        }
        s
    }
}

/// Normalize a root-relative path: forward slashes, no `.` components; rejects absolute
/// paths and any path that escapes the root (`..`). Apps that use paths as targets normalize
/// them with this before building an `Action` or a `Scope`.
pub fn normalize_rel_path(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    if p.is_absolute() || path.starts_with('~') || path.starts_with('/') || path.starts_with('\\') {
        return Err(format!(
            "path `{path}` is absolute; paths must be relative to the root"
        ));
    }
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            Component::ParentDir => {
                return Err(format!("path `{path}` escapes the root"));
            }
            _ => return Err(format!("path `{path}` is not a relative path")),
        }
    }
    if parts.is_empty() {
        return Err("empty path".to_string());
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests;
