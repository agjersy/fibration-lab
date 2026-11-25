//! The deck's own configuration, `<deck>/.present/project.toml` (contract, export presets,
//! locked slides). Providers, models, and policy presets are the session's, not the deck's.

use anyhow::Context;
use fibration_deck_protocol::config::ProjectConfig;
use std::path::Path;

pub fn load(deck_root: &Path) -> anyhow::Result<ProjectConfig> {
    let path = deck_root.join(".present").join("project.toml");
    if !path.is_file() {
        return Ok(ProjectConfig::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn save(deck_root: &Path, config: &ProjectConfig) -> anyhow::Result<()> {
    let path = deck_root.join(".present").join("project.toml");
    crate::util::write_atomic(&path, toml::to_string_pretty(config)?.as_bytes())?;
    Ok(())
}
