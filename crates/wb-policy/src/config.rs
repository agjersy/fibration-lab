// Ported from Personal Analyst `crates/protocol/src/config.rs` (`PolicyConfig` and its
// enums) at commit f229e4d.
// Adapted: adds the Present-shaped `[policy]` table (Present PROTOCOL 4.2) and conversions of
// both shapes into the generic `Policy`.
//! App-shaped `[policy]` tables and their conversion into a [`Policy`].

use crate::{EditGate, Level, LockSet, Policy, PolicyError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Policy classes used by the Present conversion (Present PROTOCOL 6.3).
pub mod present_class {
    pub const CODE: &str = "code";
    pub const SKILLS_SCRIPTS: &str = "skills_scripts";
    pub const CODE_MODE: &str = "code_mode";
    pub const EXPORT: &str = "export";
    /// The tool that `code_mode = "deny"` switches off.
    pub const SWITCH_MODE_TOOL: &str = "deck.switch_mode";
}

/// Policy classes used by the Analyst conversion (Analyst ARCHITECTURE 5.7).
pub mod analyst_class {
    pub const META: &str = "meta";
    pub const RUN: &str = "run";
    pub const PACKAGE: &str = "package";
    pub const SHELL: &str = "shell";
    pub const GIT: &str = "git";
    /// Level for runs that ask for network access; the app evaluates a second action of
    /// this class when a run sets `network: true`.
    pub const NETWORK: &str = "network";
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PresentEdits {
    #[default]
    Review,
    AutoNewSlides,
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AllowDeny {
    #[default]
    Allow,
    Deny,
}

/// `mcp = "ask"` (one level for every server) or `[policy.mcp]` with a level per server
/// (the key `default` sets the level of servers that are not listed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum McpConfig {
    Level(Level),
    PerServer(BTreeMap<String, Level>),
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig::Level(Level::Ask)
    }
}

impl McpConfig {
    /// `(default level, per-server levels)`.
    pub fn split(&self) -> (Level, BTreeMap<String, Level>) {
        match self {
            McpConfig::Level(l) => (*l, BTreeMap::new()),
            McpConfig::PerServer(m) => {
                let mut servers = m.clone();
                let default = servers.remove("default").unwrap_or(Level::Ask);
                (default, servers)
            }
        }
    }
}

/// The `[policy]` table of Present's `.present/project.toml` (PROTOCOL 4.2, ARCHITECTURE 13).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PresentPolicyConfig {
    pub edits: PresentEdits,
    /// Builds and generated slides.
    pub code: Level,
    pub skills_scripts: Level,
    /// Switching the deck to code generation mode.
    pub code_mode: AllowDeny,
    pub mcp: McpConfig,
    pub export: Level,
    /// Slide ids (or `slide/element` ids, or globs) the agent may not change.
    pub locked_slides: Vec<String>,
    pub max_steps: u32,
    /// Not a policy rule; carried so the table round-trips.
    pub build_timeout_s: u64,
}

impl Default for PresentPolicyConfig {
    fn default() -> Self {
        PresentPolicyConfig {
            edits: PresentEdits::Review,
            code: Level::Ask,
            skills_scripts: Level::Ask,
            code_mode: AllowDeny::Allow,
            mcp: McpConfig::default(),
            export: Level::Auto,
            locked_slides: vec![],
            max_steps: 60,
            build_timeout_s: 60,
        }
    }
}

impl PresentPolicyConfig {
    pub fn to_policy(&self) -> Policy {
        let (mcp_default, servers) = self.mcp.split();
        let mut p = Policy {
            edits: match self.edits {
                PresentEdits::Review => EditGate::Review,
                PresentEdits::AutoNewSlides => EditGate::AutoNew,
                PresentEdits::Auto => EditGate::Auto,
            },
            max_steps: self.max_steps,
            locked: LockSet::new(self.locked_slides.iter().cloned()),
            mcp: servers,
            ..Policy::default()
        };
        p.classes.insert(present_class::CODE.into(), self.code);
        p.classes
            .insert(present_class::SKILLS_SCRIPTS.into(), self.skills_scripts);
        p.classes.insert(crate::CLASS_MCP.into(), mcp_default);
        p.classes.insert(present_class::EXPORT.into(), self.export);
        p.classes.insert(
            present_class::CODE_MODE.into(),
            match self.code_mode {
                AllowDeny::Allow => Level::Ask,
                AllowDeny::Deny => Level::Deny,
            },
        );
        if self.code_mode == AllowDeny::Deny {
            p.denied_tools
                .insert(present_class::SWITCH_MODE_TOOL.into());
        }
        p
    }
}

impl From<&PresentPolicyConfig> for Policy {
    fn from(c: &PresentPolicyConfig) -> Self {
        c.to_policy()
    }
}

impl From<PresentPolicyConfig> for Policy {
    fn from(c: PresentPolicyConfig) -> Self {
        c.to_policy()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AnalystEdits {
    #[default]
    Review,
    AutoNewFiles,
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnalystRuns {
    #[default]
    Auto,
    Ask,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnalystNetwork {
    #[default]
    Deny,
    Ask,
    Allow,
}

/// The `[policy]` table of Analyst's `.analyst/project.toml` (Analyst ARCHITECTURE 5.8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AnalystPolicyConfig {
    pub edits: AnalystEdits,
    pub runs: AnalystRuns,
    pub packages: Level,
    pub shell: Level,
    pub network: AnalystNetwork,
    /// Commits by the agent.
    pub git: Level,
    /// Path globs the agent may never edit.
    pub protected: Vec<String>,
    pub max_steps: u32,
    /// Not a policy rule; carried so the table round-trips.
    pub max_run_seconds: u64,
    /// `[policy.mcp]`: server -> level.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, Level>,
}

impl Default for AnalystPolicyConfig {
    fn default() -> Self {
        AnalystPolicyConfig {
            edits: AnalystEdits::Review,
            runs: AnalystRuns::Auto,
            packages: Level::Ask,
            shell: Level::Ask,
            network: AnalystNetwork::Deny,
            git: Level::Ask,
            protected: vec![],
            max_steps: 40,
            max_run_seconds: 600,
            mcp: BTreeMap::new(),
        }
    }
}

impl AnalystPolicyConfig {
    /// Fails on an invalid `protected` glob, as Analyst does.
    pub fn to_policy(&self) -> Result<Policy, PolicyError> {
        let mut p = Policy {
            edits: match self.edits {
                AnalystEdits::Review => EditGate::Review,
                AnalystEdits::AutoNewFiles => EditGate::AutoNew,
                AnalystEdits::Auto => EditGate::Auto,
            },
            max_steps: self.max_steps,
            locked: LockSet::strict(self.protected.iter().cloned())?,
            mcp: self.mcp.clone(),
            ..Policy::default()
        };
        p.classes.insert(analyst_class::META.into(), Level::Auto);
        p.classes.insert(
            analyst_class::RUN.into(),
            match self.runs {
                AnalystRuns::Auto => Level::Auto,
                AnalystRuns::Ask => Level::Ask,
            },
        );
        p.classes
            .insert(analyst_class::PACKAGE.into(), self.packages);
        p.classes.insert(analyst_class::SHELL.into(), self.shell);
        p.classes.insert(analyst_class::GIT.into(), self.git);
        p.classes.insert(
            analyst_class::NETWORK.into(),
            match self.network {
                AnalystNetwork::Deny => Level::Deny,
                AnalystNetwork::Ask => Level::Ask,
                AnalystNetwork::Allow => Level::Auto,
            },
        );
        Ok(p)
    }
}

impl TryFrom<&AnalystPolicyConfig> for Policy {
    type Error = PolicyError;
    fn try_from(c: &AnalystPolicyConfig) -> Result<Self, Self::Error> {
        c.to_policy()
    }
}
