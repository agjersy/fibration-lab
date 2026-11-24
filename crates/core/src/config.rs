//! Configuration loading: global `$ANALYST_HOME/config.toml` (default `~/.analyst`), project
//! `<repo>/.analyst/project.toml`, API keys from the environment or an untracked `.env`
//! file, model spec parsing, per-target overrides (PROTOCOL 2.1, 2.2), and registration of
//! the configured network destinations with the gate.

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
    let config: GlobalConfig = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("parsing global config {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GlobalConfig::default(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    register_egress(&config)?;
    Ok(config)
}

/// Write the global config back (creates the home directory).
pub fn save_global(home: &Path, config: &GlobalConfig) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let text = toml::to_string_pretty(config).context("serializing global config")?;
    let path = global_config_path(home);
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    register_egress(config)
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

/// `<home>/.env`: where keys entered through the app are stored (mode 0600).
pub fn env_file_path(home: &Path) -> PathBuf {
    home.join(wb_net::secrets::ENV_FILE)
}

/// Where a provider's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The process environment.
    Env,
    /// A `.env` file: `$ANALYST_HOME/.env`, or the repo's own untracked `.env`.
    Stored,
}

/// The environment variable that names a provider's key: `api_key_env` when configured, else
/// the provider name uppercased with every non-alphanumeric character replaced by `_`,
/// followed by `_API_KEY` (`ollama-lab` gives `OLLAMA_LAB_API_KEY`).
pub fn key_env_name(provider: &ProviderConfig) -> String {
    match provider.api_key_env.as_deref().filter(|v| !v.is_empty()) {
        Some(v) => v.to_string(),
        None => default_key_env_name(&provider.name),
    }
}

/// The derived variable name for a provider without `api_key_env`.
pub fn default_key_env_name(provider_name: &str) -> String {
    let mut name: String = provider_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if !matches!(name.chars().next(), Some(c) if c.is_ascii_alphabetic() || c == '_') {
        name.insert(0, '_');
    }
    name.push_str("_API_KEY");
    name
}

/// The `.env` files consulted after the process environment, in order: `<home>/.env`, then
/// `<repo>/.env` when a repo is given and the file exists. A file git tracks is skipped by
/// the lookup itself, so a committed key is never used.
fn key_files(home: &Path, repo: Option<&Path>) -> Vec<PathBuf> {
    let mut files = vec![env_file_path(home)];
    if let Some(repo) = repo {
        let f = repo.join(wb_net::secrets::ENV_FILE);
        if f.is_file() {
            files.push(f);
        }
    }
    files
}

/// The key for a provider and where it came from: the process environment, then
/// `<home>/.env`, then `<repo>/.env` (when it exists and git does not track it), all under
/// the name given by [`key_env_name`].
pub fn api_key_lookup(
    home: &Path,
    repo: Option<&Path>,
    provider: &ProviderConfig,
) -> Option<(String, KeySource)> {
    let files = key_files(home, repo);
    let files: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    wb_net::secrets::lookup(&key_env_name(provider), &files).map(|(key, source)| {
        let source = match source {
            wb_net::secrets::Source::Environment => KeySource::Env,
            wb_net::secrets::Source::File(_) => KeySource::Stored,
        };
        (key, source)
    })
}

/// API key for a provider, using the default analyst home and no repo. See [`api_key_in`].
pub fn api_key(provider: &ProviderConfig) -> Result<Option<String>> {
    api_key_in(&analyst_home(), None, provider)
}

/// API key for a provider (see [`api_key_lookup`] for the order). `Ok(None)` when the
/// provider declares no key variable and none is found; an error when a variable is named
/// but no key exists anywhere.
pub fn api_key_in(
    home: &Path,
    repo: Option<&Path>,
    provider: &ProviderConfig,
) -> Result<Option<String>> {
    if let Some((key, _)) = api_key_lookup(home, repo, provider) {
        return Ok(Some(key));
    }
    match &provider.api_key_env {
        None => Ok(None),
        Some(var) => Err(anyhow!(
            "provider `{}` needs an API key: set the environment variable {var}, put `{var}=...` in {}, or store one (`analyst key set {}`, or the app's settings)",
            provider.name,
            env_file_path(home).display(),
            provider.name
        )),
    }
}

/// Store (or, with an empty key, remove) a provider's key in `<home>/.env` under
/// [`key_env_name`]. The file is created with mode 0600 and replaced atomically; other lines
/// are kept.
pub fn set_api_key(home: &Path, provider: &ProviderConfig, key: &str) -> Result<()> {
    if provider.name.is_empty() {
        return Err(anyhow!("the provider name is empty"));
    }
    let name = key_env_name(provider);
    let file = wb_net::secrets::EnvFile::in_home(home);
    let key = key.trim();
    let r = if key.is_empty() {
        file.remove(&name)
    } else {
        file.set(&name, key)
    };
    r.with_context(|| format!("updating {} ({name})", file.path().display()))
}

/// Tell the network gate which destinations the user's configuration names: every
/// provider's `base_url`, the package index, and the host of every SSH target. Called
/// whenever the global config is loaded or saved; nothing else widens the allowlist.
pub fn register_egress(global: &GlobalConfig) -> Result<()> {
    let gate = wb_net::gate();
    for p in &global.providers {
        if let Err(e) = gate.allow_model_endpoint(&p.effective_base_url()) {
            tracing::warn!("provider `{}`: {e}", p.name);
        }
    }
    crate::egress::set_package_index(&global.packages.index_url)
        .map_err(|e| anyhow!("`[packages] index_url` in config.toml: {e}"))?;
    for t in &global.targets {
        if t.kind != analyst_protocol::config::TargetKind::Ssh {
            continue;
        }
        if let Some(host) = &t.host {
            if let Err(e) = gate.allow_ssh_host(host) {
                tracing::warn!("target `{}`: {e}", t.name);
            }
        }
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
            uv_binary: None,
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
        assert!(
            api_key_in(Path::new("/nonexistent-home"), None, &r.provider)
                .unwrap()
                .is_none()
        );
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
        let other = ProviderConfig {
            name: format!("other-lab.{}", crate::util::short_rand()),
            kind: ProviderKind::OpenAiCompatible,
            base_url: None,
            api_key_env: None,
        };
        let err = api_key_in(home.path(), None, &p).unwrap_err().to_string();
        assert!(
            err.contains(&var) && err.contains("analyst key set") && err.contains(".env"),
            "{err}"
        );
        set_api_key(home.path(), &p, "sk-stored\n").unwrap();
        set_api_key(home.path(), &other, "sk-other").unwrap();
        assert_eq!(
            api_key_in(home.path(), None, &p).unwrap().as_deref(),
            Some("sk-stored")
        );
        assert_eq!(
            api_key_lookup(home.path(), None, &p).map(|(_, s)| s),
            Some(KeySource::Stored)
        );
        // Stored under the variable names, in the home's `.env`.
        let derived = key_env_name(&other);
        assert!(
            derived.starts_with("OTHER_LAB_") && derived.ends_with("_API_KEY"),
            "{derived}"
        );
        let text = std::fs::read_to_string(env_file_path(home.path())).unwrap();
        assert!(
            text.contains(&format!("{var}=\"sk-stored\""))
                && text.contains(&format!("{derived}=\"sk-other\"")),
            "{text}"
        );
        assert!(!home.path().join("credentials.toml").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(env_file_path(home.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::env::set_var(&var, "sk-env");
        assert_eq!(
            api_key_in(home.path(), None, &p).unwrap().as_deref(),
            Some("sk-env")
        );
        assert_eq!(
            api_key_lookup(home.path(), None, &p).map(|(_, s)| s),
            Some(KeySource::Env)
        );
        std::env::remove_var(&var);
        // An empty key removes the entry and leaves the others.
        set_api_key(home.path(), &p, "").unwrap();
        assert!(api_key_in(home.path(), None, &p).is_err());
        assert_eq!(
            api_key_in(home.path(), None, &other).unwrap().as_deref(),
            Some("sk-other")
        );
    }

    #[test]
    fn derived_key_names() {
        assert_eq!(default_key_env_name("ollama-lab"), "OLLAMA_LAB_API_KEY");
        assert_eq!(default_key_env_name("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(default_key_env_name("3rd.party"), "_3RD_PARTY_API_KEY");
        assert!(wb_net::secrets::valid_name(&default_key_env_name("é x")));
    }

    /// The repo's own `.env` is the last place looked, and only when git does not track it.
    #[test]
    fn repo_env_file_is_used_unless_tracked() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let var = format!(
            "ANALYST_TEST_REPO_KEY_{}",
            crate::util::short_rand().to_uppercase()
        );
        let p = ProviderConfig {
            name: "p".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: None,
            api_key_env: Some(var.clone()),
        };
        assert!(api_key_lookup(home.path(), Some(repo.path()), &p).is_none());
        std::fs::write(repo.path().join(".env"), format!("{var}=sk-repo\n")).unwrap();
        assert_eq!(
            api_key_in(home.path(), Some(repo.path()), &p)
                .unwrap()
                .as_deref(),
            Some("sk-repo")
        );
        // Without the repo it is not found; the home's file wins over the repo's.
        assert!(api_key_lookup(home.path(), None, &p).is_none());
        set_api_key(home.path(), &p, "sk-home").unwrap();
        assert_eq!(
            api_key_in(home.path(), Some(repo.path()), &p)
                .unwrap()
                .as_deref(),
            Some("sk-home")
        );
        set_api_key(home.path(), &p, "").unwrap();
        // Once git tracks the file it is ignored.
        if crate::checkpoint::git(repo.path(), &["init", "-q"], None).is_ok()
            && crate::checkpoint::git(repo.path(), &["add", "-f", ".env"], None).is_ok()
        {
            assert!(api_key_lookup(home.path(), Some(repo.path()), &p).is_none());
        }
    }

    #[test]
    fn loading_config_registers_destinations_with_the_gate() {
        let home = tempfile::tempdir().unwrap();
        let tag = crate::util::short_rand().to_lowercase();
        std::fs::write(
            global_config_path(home.path()),
            format!(
                "[packages]\nindex_url = \"https://mirror-{tag}.example/simple\"\n\n[[providers]]\nname = \"p\"\nkind = \"openai-compatible\"\nbase_url = \"http://model-{tag}.example:8080/v1\"\n\n[[targets]]\nname = \"lab\"\nkind = \"ssh\"\nhost = \"me@lab-{tag}\"\n"
            ),
        )
        .unwrap();
        let gate = wb_net::gate();
        assert!(!gate.is_allowed(wb_net::EgressClass::Ssh, &format!("lab-{tag}")));
        let g = load_global(home.path()).unwrap();
        assert_eq!(
            g.packages.index_url,
            format!("https://mirror-{tag}.example/simple")
        );
        assert!(gate.is_allowed(wb_net::EgressClass::Ssh, &format!("lab-{tag}")));
        assert!(gate.is_allowed(
            wb_net::EgressClass::PackageIndex,
            &format!("https://mirror-{tag}.example/simple")
        ));
        assert!(gate.is_allowed(
            wb_net::EgressClass::ModelEndpoint,
            &format!("http://model-{tag}.example:8080/v1")
        ));
        // A hostname nobody configured stays refused.
        assert!(gate
            .authorize_ssh(&format!("elsewhere-{tag}"), "test")
            .is_err());
        // An index that is not a URL is a configuration error, not a silent fallback.
        std::fs::write(
            global_config_path(home.path()),
            "[packages]\nindex_url = \"not a url\"\n",
        )
        .unwrap();
        assert!(load_global(home.path()).is_err());
    }
}
