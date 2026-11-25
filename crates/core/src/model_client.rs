//! The analysis side of the shared provider layer (`wb-providers`): maps the
//! `~/.fibration/config.toml` provider and model entries onto the shared crate's
//! configuration, resolves keys, and builds clients and capability records.
//!
//! Key lookup order (unchanged): the process environment, then `<home>/.env`, then the
//! repo's own untracked `.env`, all under `api_key_env` or, when that is not set, the
//! `<PROVIDER>_API_KEY` default.

use crate::config::ResolvedModel;
use anyhow::{anyhow, Result};
use fibration_protocol::config::{ModelConfig, ProviderConfig, ProviderKind, ToolSupport};
use std::path::Path;
use std::sync::Arc;
use wb_providers::config as wb;
use wb_providers::{capabilities, ClientOptions, KeyStores, ModelCapabilities, ModelClient};

/// Where keys are looked up for a session: the environment, `<home>/.env`, then the repo's
/// `.env` when a repo is given and the file exists (a file git tracks is never used).
pub fn key_stores(home: &Path, repo: Option<&Path>) -> KeyStores {
    let stores = KeyStores::for_home(home);
    match repo.map(|r| r.join(wb_net::secrets::ENV_FILE)) {
        Some(f) if f.is_file() => stores.with_project_env(f),
        _ => stores,
    }
}

/// The shared crate's provider entry for an analysis provider entry.
///
/// A key is sent when one is found: `x-api-key` for `anthropic`, `Authorization: Bearer` for
/// `openai-compatible`. An `openai-compatible` provider without `api_key_env` and without a
/// key under its default name needs none (local servers). A provider that names a variable,
/// and every `anthropic` provider, must have a key.
pub fn shared_provider(
    home: &Path,
    repo: Option<&Path>,
    provider: &ProviderConfig,
) -> Result<wb::ProviderConfig> {
    let found = crate::config::api_key_lookup(home, repo, provider).is_some();
    let var = crate::config::key_env_name(provider);
    let kind = match provider.kind {
        ProviderKind::OpenAiCompatible => wb::ProviderKind::OpenAiCompatible,
        ProviderKind::Anthropic => wb::ProviderKind::Anthropic,
    };
    if !found {
        if provider.api_key_env.is_some() {
            return Err(anyhow!(
                "provider `{}` needs an API key: set the environment variable {var}, put `{var}=...` in {}, or store one (`fibration key set {}`, or the app's settings)",
                provider.name,
                crate::config::env_file_path(home).display(),
                provider.name
            ));
        }
        if provider.kind == ProviderKind::Anthropic {
            return Err(anyhow!(
                "provider `{}` (anthropic) needs an API key: set `api_key_env` in config.toml or store a key",
                provider.name
            ));
        }
    }
    Ok(wb::ProviderConfig {
        name: provider.name.clone(),
        kind,
        base_url: provider.base_url.clone(),
        auth: if found {
            None
        } else {
            Some(wb::AuthKind::None)
        },
        key: found.then(|| format!("env:{var}")),
        headers: Default::default(),
        location: Default::default(),
    })
}

/// The shared crate's `[[models]]` override entry.
pub fn shared_overrides(m: &ModelConfig) -> wb::ModelConfig {
    wb::ModelConfig {
        provider: m.provider.clone(),
        id: m.id.clone(),
        context_window: m.context_window,
        tools: m.tools.map(shared_tools),
        vision: m.vision,
        max_output_tokens: m.max_output_tokens,
    }
}

fn shared_tools(t: ToolSupport) -> wb::ToolSupport {
    match t {
        ToolSupport::Native => wb::ToolSupport::Native,
        ToolSupport::None => wb::ToolSupport::None,
    }
}

/// The protocol's spelling of a capability record's tool support (for the UI).
pub fn tool_support(t: wb::ToolSupport) -> ToolSupport {
    match t {
        wb::ToolSupport::Native => ToolSupport::Native,
        wb::ToolSupport::None => ToolSupport::None,
    }
}

/// Build the client for a resolved model. The provider's `base_url` becomes an allowed
/// model endpoint at the network gate; nothing else widens the allowlist.
pub fn client_for_repo(
    home: &Path,
    repo: Option<&Path>,
    resolved: &ResolvedModel,
) -> Result<Arc<dyn ModelClient>> {
    let provider = shared_provider(home, repo, &resolved.provider)?;
    let keys = key_stores(home, repo);
    wb_providers::client_for(
        &provider,
        &resolved.spec.id,
        &ClientOptions {
            keys: &keys,
            audit: None,
            home: Some(home),
        },
    )
}

/// [`client_for_repo`] without a repo.
pub fn client_for_in(home: &Path, resolved: &ResolvedModel) -> Result<Arc<dyn ModelClient>> {
    client_for_repo(home, None, resolved)
}

/// The model's context window as reported by the provider, or `None` on any failure.
pub async fn model_info_window(
    home: &Path,
    repo: Option<&Path>,
    resolved: &ResolvedModel,
) -> Option<u32> {
    let provider = shared_provider(home, repo, &resolved.provider).ok()?;
    wb_providers::model_info_window(&provider, &resolved.spec.id, &key_stores(home, repo)).await
}

/// Capabilities for a resolved model, from the cache or by probing (see
/// `wb_providers::capabilities::ensure`). A model without native tool calling is an error.
pub async fn ensure_capabilities(
    home: &Path,
    client: &dyn ModelClient,
    resolved: &ResolvedModel,
    info_context_window: Option<u32>,
    force: bool,
) -> Result<ModelCapabilities> {
    let overrides = resolved.overrides.as_ref().map(shared_overrides);
    capabilities::ensure(
        home,
        &resolved.spec.to_string(),
        client,
        overrides.as_ref(),
        info_context_window,
        force,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{resolve_model, ModelSpec};
    use fibration_protocol::config::GlobalConfig;

    fn provider(text: &str) -> ProviderConfig {
        let g: GlobalConfig = toml::from_str(text).unwrap();
        g.providers[0].clone()
    }

    /// The analysis config format maps onto the shared provider entry, and the key is found
    /// in the order environment, `<home>/.env`, untracked repo `.env`.
    #[test]
    fn analysis_provider_entries_map_onto_the_shared_config() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        // A local server: no key anywhere, so no credential header.
        let local = provider(
            "[[providers]]\nname = \"mc-local-test\"\nkind = \"openai-compatible\"\nbase_url = \"http://localhost:11434/v1/\"\n",
        );
        let p = shared_provider(home.path(), None, &local).unwrap();
        assert_eq!(p.effective_auth(), wb::AuthKind::None);
        assert_eq!(p.effective_base_url(), "http://localhost:11434/v1");
        // The `<PROVIDER>_API_KEY` default: a key in the repo's untracked `.env` is used.
        std::fs::write(
            repo.path().join(".env"),
            "MC_LOCAL_TEST_API_KEY=from-repo\n",
        )
        .unwrap();
        let p = shared_provider(home.path(), Some(repo.path()), &local).unwrap();
        assert_eq!(p.key.as_deref(), Some("env:MC_LOCAL_TEST_API_KEY"));
        assert_eq!(p.effective_auth(), wb::AuthKind::Bearer);
        let keys = key_stores(home.path(), Some(repo.path()));
        assert_eq!(keys.resolve(&p).unwrap().as_deref(), Some("from-repo"));
        // `<home>/.env` wins over the repo's file.
        std::fs::write(
            home.path().join(".env"),
            "MC_LOCAL_TEST_API_KEY=from-home\n",
        )
        .unwrap();
        assert_eq!(keys.resolve(&p).unwrap().as_deref(), Some("from-home"));

        // `api_key_env` names the variable; a missing key is an error that names it.
        let named = provider(
            "[[providers]]\nname = \"gw\"\nkind = \"anthropic\"\napi_key_env = \"MC_TEST_NAMED_KEY\"\n",
        );
        let err = shared_provider(home.path(), None, &named)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("MC_TEST_NAMED_KEY") && err.contains(".env"),
            "{err}"
        );
        std::fs::write(home.path().join(".env"), "MC_TEST_NAMED_KEY=k\n").unwrap();
        let p = shared_provider(home.path(), None, &named).unwrap();
        assert_eq!(p.effective_auth(), wb::AuthKind::XApiKey);
        assert_eq!(p.key.as_deref(), Some("env:MC_TEST_NAMED_KEY"));
        // Anthropic without any key is refused before a request is built.
        let bare = provider("[[providers]]\nname = \"mc-bare-test\"\nkind = \"anthropic\"\n");
        let empty = tempfile::tempdir().unwrap();
        assert!(shared_provider(empty.path(), None, &bare)
            .unwrap_err()
            .to_string()
            .contains("needs an API key"));
    }

    #[test]
    fn model_overrides_carry_over() {
        let g: GlobalConfig = toml::from_str(
            "[[providers]]\nname = \"p\"\nkind = \"openai-compatible\"\n[[models]]\nprovider = \"p\"\nid = \"m\"\ncontext_window = 65536\ntools = \"native\"\nvision = true\nmax_output_tokens = 8000\n",
        )
        .unwrap();
        let r = resolve_model(&g, &ModelSpec::parse("p/m").unwrap()).unwrap();
        let o = shared_overrides(r.overrides.as_ref().unwrap());
        assert_eq!(o.context_window, Some(65536));
        assert_eq!(o.tools, Some(wb::ToolSupport::Native));
        assert_eq!(o.vision, Some(true));
        assert_eq!(o.max_output_tokens, Some(8000));
    }
}
