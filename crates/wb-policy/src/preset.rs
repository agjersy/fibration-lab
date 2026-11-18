//! The three policy presets (Analyst IPC.md section 8, DESIGN-REVIEW item 5). A preset is the
//! primary way to configure a policy; it expands to the detailed keys, which still override.
//!
//! Presets are written in terms of five kinds of action so that both workbenches can map
//! their own classes onto them: the analysis workbench maps file edits to `Edits` and script
//! runs to `Runs`; the deck workbench maps slide and element edits to `Edits` and deck builds
//! and skill scripts to `Runs`.

use crate::{EditGate, Level, Policy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Every edit is reviewed; runs start on their own; packages, shell and git ask.
    Review,
    /// Edits inside the declared scope are applied, edits outside it ask; runs start on
    /// their own; packages, shell and git ask.
    Auto,
    /// Edits, packages, shell and git are denied; runs of what already exists start on
    /// their own.
    ReadOnly,
}

/// What a preset decides, kind by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresetKind {
    /// Running what already exists (scripts, deck builds, skill scripts).
    Runs,
    Packages,
    Shell,
    Git,
}

/// A preset expanded to the detailed values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetFields {
    pub edits: EditGate,
    pub runs: Level,
    pub packages: Level,
    pub shell: Level,
    pub git: Level,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Review, Preset::Auto, Preset::ReadOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Review => "review",
            Preset::Auto => "auto",
            Preset::ReadOnly => "read-only",
        }
    }

    /// The edit gate of the preset.
    pub fn edits(self) -> EditGate {
        match self {
            Preset::Review => EditGate::Review,
            Preset::Auto => EditGate::Auto,
            Preset::ReadOnly => EditGate::Deny,
        }
    }

    /// The level of one kind of action.
    pub fn level(self, kind: PresetKind) -> Level {
        match (self, kind) {
            (_, PresetKind::Runs) => Level::Auto,
            (Preset::ReadOnly, _) => Level::Deny,
            (_, _) => Level::Ask,
        }
    }

    pub fn fields(self) -> PresetFields {
        PresetFields {
            edits: self.edits(),
            runs: self.level(PresetKind::Runs),
            packages: self.level(PresetKind::Packages),
            shell: self.level(PresetKind::Shell),
            git: self.level(PresetKind::Git),
        }
    }

    /// The preset these detailed values amount to, if any.
    pub fn matching(fields: &PresetFields) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.fields() == *fields)
    }

    /// Set the edit gate and the level of every mapped class on a policy. `classes` names
    /// the app's policy classes and the kind each one stands for; classes that are not
    /// listed keep their level.
    pub fn apply(self, policy: &mut Policy, classes: &[(&str, PresetKind)]) {
        policy.edits = self.edits();
        for (class, kind) in classes {
            policy
                .classes
                .insert((*class).to_string(), self.level(*kind));
        }
    }
}
