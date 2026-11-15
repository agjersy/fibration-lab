//! Configuration loading: global `$ANALYST_HOME/config.toml` (default `~/.analyst`), project
//! `<repo>/.analyst/project.toml`, API keys from environment variables, model spec parsing,
//! and per-target overrides (PROTOCOL 2.1, 2.2).

use crate::util::expand_tilde;
use analyst_protocol::config::{
    GlobalConfig, ModelConfig, ProjectConfig, ProjectSection, ProviderConfig, TargetConfig,
};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// `$ANALYST_HOME`, else `~/.analyst`.
pub fn analyst_home() -> PathBuf {
    if let Ok(h) = std::env::var("ANALYST_HOME") {
        if !h.is_empty() {
            return expand_tilde(&h);
        }
    }
    match std::env::var("HOME") {
        Ok(home) => Path::new(&home).join(".analyst"),
        Err(_) => PathBuf::from(".analyst"),
    }
}

/// Path of the global config file under a home directory.
pub fn global_config_path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// Load the global config; a missing file means all defaults.
pub fn load_global(home: &Path) -> Result<GlobalConfig> {
    let path = global_config_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("parsing global config {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GlobalConfig::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write the global config back (creates the home directory).
pub fn save_global(home: &Path, config: &GlobalConfig) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let text = toml::to_string_pretty(config).context("serializing global config")?;
    let path = global_config_path(home);
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

/// Path of the project config inside a repo.
pub fn project_config_path(repo: &Path) -> PathBuf {
    repo.join(".analyst").join("project.toml")
}

/// Load the project config; a missing file means all defaults.
pub fn load_project(repo: &Path) -> Result<ProjectConfig> {
    let path = project_config_path(repo);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("parsing project config {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProjectConfig::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// `"<provider name>/<model id>"`. The model id may itself contain slashes; the split is at
/// the first one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub provider: String,
    pub id: String,
}

impl ModelSpec {
    pub fn parse(spec: &str) -> Result<ModelSpec> {
        let (provider, id) = spec
            .split_once('/')
            .ok_or_else(|| anyhow!("model spec `{spec}` must be `<provider>/<model id>`"))?;
        if provider.is_empty() || id.is_empty() {
            return Err(anyhow!(
                "model spec `{spec}` must be `<provider>/<model id>`"
            ));
        }
        Ok(ModelSpec {
            provider: provider.to_string(),
            id: id.to_string(),
        })
    }
}

impl std::fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.id)
    }
}

/// A model spec resolved against the global config.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub spec: ModelSpec,
    pub provider: ProviderConfig,
    pub overrides: Option<ModelConfig>,
}

/// Find the provider (and any `[[models]]` override) for a spec.
pub fn resolve_model(global: &GlobalConfig, spec: &ModelSpec) -> Result<ResolvedModel> {
    let provider = global
        .providers
        .iter()
        .find(|p| p.name == spec.provider)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "provider `{}` is not configured (known: {})",
                spec.provider,
                global
                    .providers
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let overrides = global
        .models
        .iter()
        .find(|m| m.provider == spec.provider && m.id == spec.id)
        .cloned();
    Ok(ResolvedModel {
        spec: spec.clone(),
        provider,
        overrides,
    })
}

/// The default model spec from config, or an error naming the file to edit.
pub fn default_model(global: &GlobalConfig) -> Result<ModelSpec> {
    let s = global.defaults.model.as_deref().ok_or_else(|| {
        anyhow!("no model given and `[defaults].model` is not set in config.toml")
    })?;
    ModelSpec::parse(s)
}

/// Path of the stored API keys under a home directory.
pub fn credentials_path(home: &Path) -> PathBuf {
    home.join("credentials.toml")
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Credentials {
    #[serde(default)]
    keys: std::collections::BTreeMap<String, String>,
}

fn load_credentials(home: &Path) -> Result<Credentials> {
    let path = credentials_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Where a provider's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The environment variable named by `api_key_env`.
    Env,
    /// `$ANALYST_HOME/credentials.toml`.
    Stored,
}

/// The key for a provider and where it came from: the environment variable named by
/// `api_key_env` wins when set and non-empty, then `[keys] <provider name>` in
/// `<home>/credentials.toml`.
pub fn api_key_lookup(home: &Path, provider: &ProviderConfig) -> Option<(String, KeySource)> {
    if let Some(var) = &provider.api_key_env {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return Some((v, KeySource::Env));
            }
        }
    }
    load_credentials(home)
        .ok()
        .and_then(|c| c.keys.get(&provider.name).cloned())
        .filter(|k| !k.is_empty())
        .map(|k| (k, KeySource::Stored))
}

/// API key for a provider, using the default analyst home for stored keys. See
/// [`api_key_in`].
pub fn api_key(provider: &ProviderConfig) -> Result<Option<String>> {
    api_key_in(&analyst_home(), provider)
}

/// API key for a provider: environment variable first, then `<home>/credentials.toml`.
/// `Ok(None)` when the provider declares no key variable and none is stored; an error when a
/// variable is named but neither it nor a stored key exists.
pub fn api_key_in(home: &Path, provider: &ProviderConfig) -> Result<Option<String>> {
    if let Some((key, _)) = api_key_lookup(home, provider) {
        return Ok(Some(key));
    }
    match &provider.api_key_env {
        None => Ok(None),
        Some(var) => Err(anyhow!(
            "provider `{}` needs an API key: set the environment variable {var}, or store one (`analyst key set {}`, or the app's settings)",
            provider.name,
            provider.name
        )),
    }
}

/// Store (or, with an empty key, remove) a provider's key in `<home>/credentials.toml`. The
/// file is created with mode 0600 and replaced atomically.
pub fn set_api_key(home: &Path, provider: &str, key: &str) -> Result<()> {
    if provider.is_empty() {
        return Err(anyhow!("the provider name is empty"));
    }
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let mut creds = load_credentials(home)?;
    let key = key.trim();
    if key.is_empty() {
        creds.keys.remove(provider);
    } else {
        creds.keys.insert(provider.to_string(), key.to_string());
    }
    let text = toml::to_string_pretty(&creds).context("serializing credentials")?;
    let path = credentials_path(home);
    let tmp = home.join(format!(".credentials.{}.tmp", crate::util::short_rand()));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(text.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

/// Look up a target by name. `local` always exists.
pub fn find_target(global: &GlobalConfig, name: &str) -> Result<TargetConfig> {
    if name == "local" {
        return Ok(TargetConfig {
            name: "local".into(),
            kind: analyst_protocol::config::TargetKind::Local,
            host: None,
            remote_root: None,
            python: None,
            sandbox: None,
            data_dirs: vec![],
        });
    }
    global
        .targets
        .iter()
        .find(|t| t.name == name)
        .cloned()
        .ok_or_else(|| anyhow!("target `{name}` is not configured in config.toml"))
}

/// The project section with per-target overrides applied.
pub fn effective_project(project: &ProjectConfig, target: &str) -> ProjectSection {
    let mut section = project.project.clone();
    if let Some(o) = project.targets.get(target) {
        if let Some(d) = &o.data_dirs {
            section.data_dirs = d.clone();
        }
        if let Some(t) = o.timeout_s {
            section.timeout_s = t;
        }
        if let Some(out) = &o.out_dir {
            section.out_dir = out.clone();
        }
    }
    section
}

#[cfg(test)]
mod tests {
    use super::*;
    use analyst_protocol::config::{ProviderKind, TargetOverride};

    #[test]
    fn model_spec_parsing() {
        let s = ModelSpec::parse("ollama/meta/llama-3").unwrap();
        assert_eq!(s.provider, "ollama");
        assert_eq!(s.id, "meta/llama-3");
        assert!(ModelSpec::parse("nope").is_err());
        assert!(ModelSpec::parse("/x").is_err());
    }

    #[test]
    fn resolve_and_overrides() {
        let mut g = GlobalConfig::default();
        g.providers.push(ProviderConfig {
            name: "p".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: Some("http://x/v1".into()),
            api_key_env: None,
        });
        g.models.push(ModelConfig {
            provider: "p".into(),
            id: "m".into(),
            context_window: Some(1000),
            tools: None,
            vision: None,
            max_output_tokens: None,
        });
        let r = resolve_model(&g, &ModelSpec::parse("p/m").unwrap()).unwrap();
        assert_eq!(r.overrides.unwrap().context_window, Some(1000));
        assert!(resolve_model(&g, &ModelSpec::parse("q/m").unwrap()).is_err());
        assert!(api_key(&r.provider).unwrap().is_none());
    }

    #[test]
    fn loads_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_global(dir.path()).unwrap(), GlobalConfig::default());
        assert_eq!(load_project(dir.path()).unwrap(), ProjectConfig::default());
    }

    #[test]
    fn per_target_overrides() {
        let mut p = ProjectConfig::default();
        p.targets.insert(
            "lab".into(),
            TargetOverride {
                data_dirs: Some(vec!["/data".into()]),
                timeout_s: Some(5),
                out_dir: None,
            },
        );
        let e = effective_project(&p, "lab");
        assert_eq!(e.data_dirs, vec!["/data"]);
        assert_eq!(e.timeout_s, 5);
        assert_eq!(e.out_dir, "out");
        assert_eq!(effective_project(&p, "local").timeout_s, 600);
    }

    /// B12: keys can be stored, the environment wins, and the file is private.
    #[test]
    fn stored_api_keys() {
        let home = tempfile::tempdir().unwrap();
        let var = format!(
            "ANALYST_TEST_KEY_{}",
            crate::util::short_rand().to_uppercase()
        );
        let p = ProviderConfig {
            name: "anthropic".into(),
            kind: ProviderKind::Anthropic,
            base_url: None,
            api_key_env: Some(var.clone()),
        };
        let err = api_key_in(home.path(), &p).unwrap_err().to_string();
        assert!(
            err.contains(&var) && err.contains("analyst key set"),
            "{err}"
        );
        set_api_key(home.path(), "anthropic", "sk-stored\n").unwrap();
        set_api_key(home.path(), "other", "sk-other").unwrap();
        assert_eq!(
            api_key_in(home.path(), &p).unwrap().as_deref(),
            Some("sk-stored")
        );
        assert_eq!(
            api_key_lookup(home.path(), &p).map(|(_, s)| s),
            Some(KeySource::Stored)
        );
        let text = std::fs::read_to_string(credentials_path(home.path())).unwrap();
        assert!(
            text.contains("[keys]") && text.contains("anthropic = \"sk-stored\""),
            "{text}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(credentials_path(home.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::env::set_var(&var, "sk-env");
        assert_eq!(
            api_key_in(home.path(), &p).unwrap().as_deref(),
            Some("sk-env")
        );
        assert_eq!(
            api_key_lookup(home.path(), &p).map(|(_, s)| s),
            Some(KeySource::Env)
        );
        std::env::remove_var(&var);
        // An empty key removes the entry and leaves the others.
        set_api_key(home.path(), "anthropic", "").unwrap();
        assert!(api_key_in(home.path(), &p).is_err());
        let other = ProviderConfig {
            name: "other".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: None,
            api_key_env: None,
        };
        assert_eq!(
            api_key_in(home.path(), &other).unwrap().as_deref(),
            Some("sk-other")
        );
    }
}
