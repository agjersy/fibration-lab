//! Home directory resolution and configuration loading (PROTOCOL 4.1, 4.2).

use analyst_deck_protocol::config::{GlobalConfig, ProjectConfig};
use anyhow::Context;
use std::path::{Path, PathBuf};

/// `PRESENT_HOME`, else `%APPDATA%\Present` on Windows, else `~/.present`.
pub fn default_home() -> PathBuf {
    if let Ok(home) = std::env::var("PRESENT_HOME") {
        return PathBuf::from(home);
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Present");
        }
    }
    let base = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join(".present")
}

pub fn load_global(home: &Path) -> anyhow::Result<GlobalConfig> {
    let path = home.join("config.toml");
    if !path.is_file() {
        return Ok(GlobalConfig::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn load_project(project: &Path) -> anyhow::Result<ProjectConfig> {
    let path = project.join(".present").join("project.toml");
    if !path.is_file() {
        return Ok(ProjectConfig::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn save_project(project: &Path, config: &ProjectConfig) -> anyhow::Result<()> {
    let path = project.join(".present").join("project.toml");
    crate::util::write_atomic(&path, toml::to_string_pretty(config)?.as_bytes())?;
    Ok(())
}
