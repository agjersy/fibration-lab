// Ported from Personal Analyst `crates/protocol/src/config.rs` (`PolicyConfig` and its
// enums) at commit f229e4d.
// Adapted: adds the deck-shaped `[policy]` table (Present PROTOCOL 4.2), the `preset` key
// in both tables, and conversions of both shapes into the generic `Policy`.
//! App-shaped `[policy]` tables and their conversion into a [`Policy`].
//!
//! Both tables take `preset = "review" | "auto" | "read-only"`. The preset expands to the
//! detailed keys; a detailed key written in the same table overrides the preset's value.
//! Without a preset the table's own defaults apply. Keys this crate does not know (`mcp`
//! and `network` from older files) are ignored.

use crate::preset::{Preset, PresetKind};
use crate::session::Unsandboxed;
use crate::{EditGate, Level, LockSet, Policy, PolicyError};
use serde::{Deserialize, Serialize};

/// Policy classes used by the deck conversion (Present PROTOCOL 6.3).
pub mod present_class {
    use crate::preset::PresetKind;

    pub const CODE: &str = "code";
    pub const SKILLS_SCRIPTS: &str = "skills_scripts";
    pub const CODE_MODE: &str = "code_mode";
    pub const EXPORT: &str = "export";
    /// The tool that `code_mode = "deny"` switches off.
    pub const SWITCH_MODE_TOOL: &str = "deck.switch_mode";
    /// How the presets gate deck actions: slide and element edits are the edit gate; deck
    /// builds (and generated slides) and skill scripts are runs.
    pub const PRESET_CLASSES: &[(&str, PresetKind)] =
        &[(CODE, PresetKind::Runs), (SKILLS_SCRIPTS, PresetKind::Runs)];
}

/// Policy classes used by the analysis conversion (Analyst ARCHITECTURE 5.7).
pub mod analyst_class {
    use crate::preset::PresetKind;

    pub const META: &str = "meta";
    pub const RUN: &str = "run";
    pub const PACKAGE: &str = "package";
    pub const SHELL: &str = "shell";
    pub const GIT: &str = "git";
    /// How the presets gate analysis actions.
    pub const PRESET_CLASSES: &[(&str, PresetKind)] = &[
        (RUN, PresetKind::Runs),
        (PACKAGE, PresetKind::Packages),
        (SHELL, PresetKind::Shell),
        (GIT, PresetKind::Git),
    ];
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PresentEdits {
    #[default]
    Review,
    AutoNewSlides,
    Auto,
    Deny,
}

impl PresentEdits {
    pub fn gate(self) -> EditGate {
        match self {
            PresentEdits::Review => EditGate::Review,
            PresentEdits::AutoNewSlides => EditGate::AutoNew,
            PresentEdits::Auto => EditGate::Auto,
            PresentEdits::Deny => EditGate::Deny,
        }
    }
    fn of(gate: EditGate) -> Self {
        match gate {
            EditGate::Review => PresentEdits::Review,
            EditGate::AutoNew => PresentEdits::AutoNewSlides,
            EditGate::Auto => PresentEdits::Auto,
            EditGate::Deny => PresentEdits::Deny,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AllowDeny {
    #[default]
    Allow,
    Deny,
}

/// The `[policy]` table of the deck's `.present/project.toml` (PROTOCOL 4.2).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PresentPolicyConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    pub edits: PresentEdits,
    /// Builds and generated slides.
    pub code: Level,
    pub skills_scripts: Level,
    /// Switching the deck to code generation mode.
    pub code_mode: AllowDeny,
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
            preset: None,
            edits: PresentEdits::Review,
            code: Level::Ask,
            skills_scripts: Level::Ask,
            code_mode: AllowDeny::Allow,
            export: Level::Auto,
            locked_slides: vec![],
            max_steps: 60,
            build_timeout_s: 60,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PresentPolicyRaw {
    preset: Option<Preset>,
    edits: Option<PresentEdits>,
    code: Option<Level>,
    skills_scripts: Option<Level>,
    code_mode: Option<AllowDeny>,
    export: Option<Level>,
    locked_slides: Option<Vec<String>>,
    max_steps: Option<u32>,
    build_timeout_s: Option<u64>,
}

impl PresentPolicyConfig {
    /// The table a preset stands for: edits follow the preset, deck builds and skill
    /// scripts are runs, and `read-only` also refuses the switch to code generation mode
    /// (it changes the deck). Everything else keeps the table default.
    pub fn for_preset(preset: Preset) -> Self {
        PresentPolicyConfig {
            preset: Some(preset),
            edits: PresentEdits::of(preset.edits()),
            code: preset.level(PresetKind::Runs),
            skills_scripts: preset.level(PresetKind::Runs),
            code_mode: if preset == Preset::ReadOnly {
                AllowDeny::Deny
            } else {
                AllowDeny::Allow
            },
            ..Default::default()
        }
    }

    pub fn to_policy(&self) -> Policy {
        let mut p = Policy {
            edits: self.edits.gate(),
            max_steps: self.max_steps,
            locked: LockSet::new(self.locked_slides.iter().cloned()),
            ..Policy::default()
        };
        p.classes.insert(present_class::CODE.into(), self.code);
        p.classes
            .insert(present_class::SKILLS_SCRIPTS.into(), self.skills_scripts);
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

impl<'de> Deserialize<'de> for PresentPolicyConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = PresentPolicyRaw::deserialize(d)?;
        let mut c = match raw.preset {
            Some(p) => PresentPolicyConfig::for_preset(p),
            None => PresentPolicyConfig::default(),
        };
        if let Some(v) = raw.edits {
            c.edits = v;
        }
        if let Some(v) = raw.code {
            c.code = v;
        }
        if let Some(v) = raw.skills_scripts {
            c.skills_scripts = v;
        }
        if let Some(v) = raw.code_mode {
            c.code_mode = v;
        }
        if let Some(v) = raw.export {
            c.export = v;
        }
        if let Some(v) = raw.locked_slides {
            c.locked_slides = v;
        }
        if let Some(v) = raw.max_steps {
            c.max_steps = v;
        }
        if let Some(v) = raw.build_timeout_s {
            c.build_timeout_s = v;
        }
        Ok(c)
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
    Deny,
}

impl AnalystEdits {
    pub fn gate(self) -> EditGate {
        match self {
            AnalystEdits::Review => EditGate::Review,
            AnalystEdits::AutoNewFiles => EditGate::AutoNew,
            AnalystEdits::Auto => EditGate::Auto,
            AnalystEdits::Deny => EditGate::Deny,
        }
    }
    fn of(gate: EditGate) -> Self {
        match gate {
            EditGate::Review => AnalystEdits::Review,
            EditGate::AutoNew => AnalystEdits::AutoNewFiles,
            EditGate::Auto => AnalystEdits::Auto,
            EditGate::Deny => AnalystEdits::Deny,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnalystRuns {
    #[default]
    Auto,
    Ask,
}

/// The `[policy]` table of the analysis `.analyst/project.toml` (Analyst PROTOCOL 2.2).
/// There is no network key: runs never have network access.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AnalystPolicyConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    pub edits: AnalystEdits,
    pub runs: AnalystRuns,
    pub packages: Level,
    pub shell: Level,
    /// Commits by the agent.
    pub git: Level,
    /// Path globs the agent may never edit.
    pub protected: Vec<String>,
    pub max_steps: u32,
    /// What run and shell tools do when no OS sandbox confines them.
    pub unsandboxed_runs: Unsandboxed,
}

impl Default for AnalystPolicyConfig {
    /// The defaults are the `review` preset.
    fn default() -> Self {
        AnalystPolicyConfig {
            preset: None,
            edits: AnalystEdits::Review,
            runs: AnalystRuns::Auto,
            packages: Level::Ask,
            shell: Level::Ask,
            git: Level::Ask,
            protected: vec![],
            max_steps: 40,
            unsandboxed_runs: Unsandboxed::Ask,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AnalystPolicyRaw {
    preset: Option<Preset>,
    edits: Option<AnalystEdits>,
    runs: Option<AnalystRuns>,
    packages: Option<Level>,
    shell: Option<Level>,
    git: Option<Level>,
    protected: Option<Vec<String>>,
    max_steps: Option<u32>,
    unsandboxed_runs: Option<Unsandboxed>,
}

impl<'de> Deserialize<'de> for AnalystPolicyConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = AnalystPolicyRaw::deserialize(d)?;
        let mut c = match raw.preset {
            Some(p) => AnalystPolicyConfig::for_preset(p),
            None => AnalystPolicyConfig::default(),
        };
        if let Some(v) = raw.edits {
            c.edits = v;
        }
        if let Some(v) = raw.runs {
            c.runs = v;
        }
        if let Some(v) = raw.packages {
            c.packages = v;
        }
        if let Some(v) = raw.shell {
            c.shell = v;
        }
        if let Some(v) = raw.git {
            c.git = v;
        }
        if let Some(v) = raw.protected {
            c.protected = v;
        }
        if let Some(v) = raw.max_steps {
            c.max_steps = v;
        }
        if let Some(v) = raw.unsandboxed_runs {
            c.unsandboxed_runs = v;
        }
        Ok(c)
    }
}

impl AnalystPolicyConfig {
    /// The table a preset stands for.
    pub fn for_preset(preset: Preset) -> Self {
        let f = preset.fields();
        AnalystPolicyConfig {
            preset: Some(preset),
            edits: AnalystEdits::of(f.edits),
            runs: match f.runs {
                Level::Auto => AnalystRuns::Auto,
                _ => AnalystRuns::Ask,
            },
            packages: f.packages,
            shell: f.shell,
            git: f.git,
            ..Default::default()
        }
    }

    /// Set the five preset keys (and remember the preset); protected globs, the step limit
    /// and the unsandboxed setting are left as they are.
    pub fn apply_preset(&mut self, preset: Preset) {
        let p = AnalystPolicyConfig::for_preset(preset);
        self.preset = Some(preset);
        self.edits = p.edits;
        self.runs = p.runs;
        self.packages = p.packages;
        self.shell = p.shell;
        self.git = p.git;
    }

    /// The preset the five keys currently amount to, or `None` when detailed keys were
    /// changed away from all three.
    pub fn effective_preset(&self) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| {
            let f = AnalystPolicyConfig::for_preset(*p);
            (f.edits, f.runs, f.packages, f.shell, f.git)
                == (self.edits, self.runs, self.packages, self.shell, self.git)
        })
    }

    /// Fails on an invalid `protected` glob.
    pub fn to_policy(&self) -> Result<Policy, PolicyError> {
        let mut p = Policy {
            edits: self.edits.gate(),
            max_steps: self.max_steps,
            locked: LockSet::strict(self.protected.iter().cloned())?,
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
        Ok(p)
    }
}

impl TryFrom<&AnalystPolicyConfig> for Policy {
    type Error = PolicyError;
    fn try_from(c: &AnalystPolicyConfig) -> Result<Self, Self::Error> {
        c.to_policy()
    }
}
