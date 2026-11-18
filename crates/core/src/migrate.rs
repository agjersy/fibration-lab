//! One-time migration from the deck workbench's old home (`~/.present`) into the Personal
//! Analyst home (`~/.analyst`, or `$ANALYST_HOME`).
//!
//! On first start the entry points (the `analyst` CLI and the desktop shell) call
//! [`migrate_legacy_home`]. When `~/.present` exists and the home has no marker yet, it:
//!
//! - merges the old `[[providers]]` into `config.toml`, skipping names that already exist
//!   (`key = "env:VAR"` becomes `api_key_env = "VAR"`);
//! - copies `skills/` and `templates/`, never overwriting a file that is already there;
//! - copies the keys of the old `.env` that are not set in the new one;
//! - writes the marker `.present-migrated`, so it runs once.
//!
//! `~/.present` itself is only read, never changed or deleted. The old home override variable
//! is not consulted: the old home is always `$HOME/.present`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The marker file under the analyst home.
pub const MARKER: &str = ".present-migrated";

/// What a migration did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub from: PathBuf,
    /// Providers added to `config.toml`.
    pub providers: Vec<String>,
    /// Providers left out because the analyst config already has one of that name.
    pub skipped_providers: Vec<String>,
    /// Files copied, relative to the old home (`skills/...`, `templates/...`).
    pub files: Vec<String>,
    /// Key names copied into the analyst `.env`.
    pub keys: Vec<String>,
}

/// The old home: `$HOME/.present`.
pub fn legacy_home() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".present"))
}

/// Migrate `~/.present` into `home` once. Returns `None` when there is nothing to do (no old
/// home, or already migrated).
pub fn migrate_legacy_home(home: &Path) -> Result<Option<MigrationReport>> {
    match legacy_home() {
        Some(legacy) => migrate_from(home, &legacy),
        None => Ok(None),
    }
}

/// Migrate `legacy` into `home` unless `home` carries the marker or `legacy` does not exist.
pub fn migrate_from(home: &Path, legacy: &Path) -> Result<Option<MigrationReport>> {
    let marker = home.join(MARKER);
    if marker.exists() || !legacy.is_dir() {
        return Ok(None);
    }
    // The old and new home are the same directory: nothing to merge.
    if let (Ok(a), Ok(b)) = (legacy.canonicalize(), home.canonicalize()) {
        if a == b {
            return Ok(None);
        }
    }
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let mut report = MigrationReport {
        from: legacy.to_path_buf(),
        ..Default::default()
    };
    merge_providers(home, legacy, &mut report)?;
    for dir in ["skills", "templates"] {
        copy_missing(&legacy.join(dir), &home.join(dir), dir, &mut report.files)?;
    }
    copy_keys(home, legacy, &mut report)?;
    let summary = format!(
        "migrated from {} at {}\nproviders: {}\nskipped providers: {}\nfiles: {}\nkeys: {}\n",
        legacy.display(),
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        report.providers.join(", "),
        report.skipped_providers.join(", "),
        report.files.len(),
        report.keys.join(", "),
    );
    std::fs::write(&marker, summary).with_context(|| format!("writing {}", marker.display()))?;
    Ok(Some(report))
}

fn read_toml(path: &Path) -> Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// One old provider table as an analyst provider table, or `None` when it lacks a name or a
/// kind the analyst knows.
fn convert_provider(old: &toml::Table) -> Option<toml::Table> {
    let name = old.get("name")?.as_str()?.to_string();
    let kind = old.get("kind")?.as_str()?;
    if !matches!(kind, "anthropic" | "openai-compatible") {
        return None;
    }
    let mut new = toml::Table::new();
    new.insert("name".into(), toml::Value::String(name));
    new.insert("kind".into(), toml::Value::String(kind.to_string()));
    if let Some(url) = old.get("base_url").and_then(|v| v.as_str()) {
        new.insert("base_url".into(), toml::Value::String(url.to_string()));
    }
    if let Some(var) = old
        .get("key")
        .and_then(|v| v.as_str())
        .and_then(|k| k.strip_prefix("env:"))
        .filter(|v| wb_net::secrets::valid_name(v))
    {
        new.insert("api_key_env".into(), toml::Value::String(var.to_string()));
    }
    Some(new)
}

fn merge_providers(home: &Path, legacy: &Path, report: &mut MigrationReport) -> Result<()> {
    let old = read_toml(&legacy.join("config.toml"))?;
    let old_providers: Vec<toml::Table> = old
        .get("providers")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_table().cloned())
        .collect();
    if old_providers.is_empty() {
        return Ok(());
    }
    let path = crate::config::global_config_path(home);
    let mut config = read_toml(&path)?;
    let mut providers: Vec<toml::Value> = config
        .get("providers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut added = false;
    for old in &old_providers {
        let Some(new) = convert_provider(old) else {
            continue;
        };
        let name = new["name"].as_str().unwrap_or_default().to_string();
        let exists = providers
            .iter()
            .any(|p| p.get("name").and_then(|n| n.as_str()) == Some(name.as_str()));
        if exists {
            report.skipped_providers.push(name);
        } else {
            providers.push(toml::Value::Table(new));
            report.providers.push(name);
            added = true;
        }
    }
    if added {
        config.insert("providers".into(), toml::Value::Array(providers));
        let text = toml::to_string_pretty(&config).context("serializing config.toml")?;
        // The merged file must still load as an analyst config.
        toml::from_str::<analyst_protocol::config::GlobalConfig>(&text)
            .context("the merged config.toml does not load")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Copy every file under `from` to the same place under `to`, keeping files that exist.
fn copy_missing(from: &Path, to: &Path, prefix: &str, copied: &mut Vec<String>) -> Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(from)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        let rel = match entry.path().strip_prefix(from) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let target = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() && !target.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
            copied.push(format!(
                "{prefix}/{}",
                rel.to_string_lossy().replace('\\', "/")
            ));
        }
    }
    Ok(())
}

fn copy_keys(home: &Path, legacy: &Path, report: &mut MigrationReport) -> Result<()> {
    let text = match std::fs::read_to_string(legacy.join(wb_net::secrets::ENV_FILE)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("reading the old .env"),
    };
    let target = wb_net::secrets::EnvFile::in_home(home);
    for (name, value) in wb_net::secrets::parse(&text) {
        if value.is_empty() || !wb_net::secrets::valid_name(&name) {
            continue;
        }
        if target.get(&name)?.is_some() || report.keys.contains(&name) {
            continue;
        }
        target
            .set(&name, &value)
            .with_context(|| format!("writing {}", target.path().display()))?;
        report.keys.push(name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_home_fixture(dir: &Path) {
        std::fs::create_dir_all(dir.join("skills/charts")).unwrap();
        std::fs::write(dir.join("skills/charts/SKILL.md"), "old charts skill").unwrap();
        std::fs::create_dir_all(dir.join("templates")).unwrap();
        std::fs::write(dir.join("templates/corp.potx"), b"potx").unwrap();
        std::fs::write(
            dir.join("config.toml"),
            r#"
[defaults]
model = "work/claude"

[[providers]]
name = "work"
kind = "anthropic"
key = "env:WORK_KEY"
location = "external"

[[providers]]
name = "local"
kind = "openai-compatible"
base_url = "http://127.0.0.1:11434/v1"
auth = "none"

[[providers]]
name = "odd"
kind = "something-else"
"#,
        )
        .unwrap();
        std::fs::write(dir.join(".env"), "WORK_KEY=sk-old\nLOCAL_API_KEY=abc\n").unwrap();
    }

    #[test]
    fn merges_providers_files_and_keys_once() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("present-home");
        let home = tmp.path().join("analyst-home");
        legacy_home_fixture(&legacy);
        std::fs::create_dir_all(home.join("skills/charts")).unwrap();
        std::fs::write(home.join("skills/charts/SKILL.md"), "new charts skill").unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[defaults]\nmodel = \"local/llama\"\n\n[[providers]]\nname = \"local\"\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9999/v1\"\n",
        )
        .unwrap();
        std::fs::write(home.join(".env"), "LOCAL_API_KEY=kept\n").unwrap();

        let report = migrate_from(&home, &legacy).unwrap().expect("migrated");
        assert_eq!(report.providers, vec!["work".to_string()]);
        assert_eq!(report.skipped_providers, vec!["local".to_string()]);
        assert_eq!(report.keys, vec!["WORK_KEY".to_string()]);
        assert!(report.files.contains(&"templates/corp.potx".to_string()));
        assert!(!report.files.iter().any(|f| f.starts_with("skills/")));

        let config = crate::config::load_global(&home).unwrap();
        assert_eq!(config.defaults.model.as_deref(), Some("local/llama"));
        let work = config.providers.iter().find(|p| p.name == "work").unwrap();
        assert_eq!(work.api_key_env.as_deref(), Some("WORK_KEY"));
        let local = config.providers.iter().find(|p| p.name == "local").unwrap();
        assert_eq!(local.base_url.as_deref(), Some("http://127.0.0.1:9999/v1"));
        assert!(!config.providers.iter().any(|p| p.name == "odd"));

        assert_eq!(
            std::fs::read_to_string(home.join("skills/charts/SKILL.md")).unwrap(),
            "new charts skill"
        );
        assert!(home.join("templates/corp.potx").is_file());
        let env = wb_net::secrets::EnvFile::in_home(&home);
        assert_eq!(env.get("WORK_KEY").unwrap().as_deref(), Some("sk-old"));
        assert_eq!(env.get("LOCAL_API_KEY").unwrap().as_deref(), Some("kept"));
        assert!(home.join(MARKER).is_file());

        // The old home is untouched, and a second start does nothing.
        assert!(legacy.join("config.toml").is_file() && legacy.join(".env").is_file());
        std::fs::write(legacy.join("templates/new.potx"), b"x").unwrap();
        assert_eq!(migrate_from(&home, &legacy).unwrap(), None);
        assert!(!home.join("templates/new.potx").exists());
    }

    #[test]
    fn fresh_home_gets_the_old_providers() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("present-home");
        let home = tmp.path().join("analyst-home");
        legacy_home_fixture(&legacy);
        let report = migrate_from(&home, &legacy).unwrap().expect("migrated");
        assert_eq!(
            report.providers,
            vec!["work".to_string(), "local".to_string()]
        );
        let config = crate::config::load_global(&home).unwrap();
        assert_eq!(config.providers.len(), 2);
        assert!(home.join("skills/charts/SKILL.md").is_file());
        assert_eq!(report.keys.len(), 2);
    }

    #[test]
    fn nothing_to_do_without_an_old_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("analyst-home");
        assert_eq!(
            migrate_from(&home, &tmp.path().join("missing")).unwrap(),
            None
        );
        assert!(!home.join(MARKER).exists());
    }
}
