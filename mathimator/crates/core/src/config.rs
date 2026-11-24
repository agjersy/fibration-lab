//! Standalone configuration (ARCHITECTURE 14): the Mathimator home and its model providers.
//!
//! `<home>/config.toml` holds providers in the shared `wb-providers` format, the same one
//! Personal Analyst uses, and the default model:
//!
//! ```toml
//! default_model = "anthropic/claude-opus-5-5"     # "<provider name>/<model id>"
//!
//! [[providers]]
//! name = "anthropic"
//! kind = "anthropic"
//! key = "env:ANTHROPIC_API_KEY"                   # read from the environment, then <home>/.env
//!
//! [[providers]]
//! name = "local"
//! kind = "openai-compatible"
//! base_url = "http://localhost:11434/v1"
//! ```
//!
//! Keys are never written here. A provider's base URL becomes an allowed endpoint at the
//! network gate (`wb-net`), which records every request in `<home>/egress.jsonl`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use wb_providers::{ClientOptions, KeyStores, ModelClient, ProviderConfig};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

/// `MATHIMATOR_HOME`, else `~/.mathimator`.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("MATHIMATOR_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".mathimator")
}

/// Read `<home>/config.toml`; a missing file is an empty configuration.
pub fn load(home: &Path) -> Result<Config> {
    let path = home.join("config.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Split `"<provider>/<model id>"`. The model id may itself contain slashes.
pub fn split_model(spec: &str) -> Result<(&str, &str)> {
    match spec.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => Ok((provider, model)),
        _ => bail!("a model is written `<provider>/<model id>`, for example anthropic/claude-opus-5-5; got `{spec}`"),
    }
}

/// The client for `model` (or the configured default). Initialises the network gate for
/// `home` on first use.
pub fn client(home: &Path, config: &Config, model: Option<&str>) -> Result<Arc<dyn ModelClient>> {
    let spec = model.or(config.default_model.as_deref()).ok_or_else(|| {
        anyhow!(
            "no model is configured: pass --model <provider>/<model id>, or set default_model in {}",
            home.join("config.toml").display()
        )
    })?;
    let (provider_name, model_id) = split_model(spec)?;
    let provider = config
        .providers
        .iter()
        .find(|p| p.name == provider_name)
        .ok_or_else(|| {
            let known: Vec<&str> = config.providers.iter().map(|p| p.name.as_str()).collect();
            anyhow!(
                "there is no provider `{provider_name}` in {} (configured: {})",
                home.join("config.toml").display(),
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )
        })?;
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    wb_net::init(home);
    let keys = KeyStores::for_home(home);
    wb_providers::client_for(
        provider,
        model_id,
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: Some(home),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_the_shared_provider_format() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), Config::default());
        std::fs::write(
            dir.path().join("config.toml"),
            "default_model = \"local/qwen3:32b\"\n[[providers]]\nname = \"local\"\nkind = \"openai-compatible\"\nbase_url = \"http://localhost:11434/v1\"\n",
        )
        .unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.providers[0].name, "local");
        assert_eq!(
            split_model(cfg.default_model.as_deref().unwrap()).unwrap(),
            ("local", "qwen3:32b")
        );
        assert_eq!(split_model("or/meta/llama").unwrap(), ("or", "meta/llama"));
        assert!(split_model("nomodel").is_err());
    }

    #[test]
    fn a_missing_model_or_provider_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let err = client(dir.path(), &Config::default(), None)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("--model"), "{err}");
        let err = client(dir.path(), &Config::default(), Some("x/y"))
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("no provider `x`") && err.contains("none"),
            "{err}"
        );
    }
}
