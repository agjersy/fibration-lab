//! Turning configuration into a working agent: model resolution, policy, skills.

use crate::agent::tools::Mode;
use crate::agent::{Agent, AgentConfig, Approver};
use crate::audit::AuditLog;
use crate::workspace::Workspace;
use anyhow::{anyhow, Context, Result};
use present_protocol::config::{GlobalConfig, ProjectConfig};
use std::path::Path;
use std::sync::Arc;
use wb_policy::{Policy, PresentPolicyConfig};
use wb_providers::model::ModelCapabilities;
use wb_providers::{capabilities, ClientOptions, ModelClient};
use wb_skills::{SkillRoot, SkillSet, SkillSource};
use wb_transcript::TranscriptStore;

pub struct ResolvedModel {
    pub client: Arc<dyn ModelClient>,
    pub caps: ModelCapabilities,
    /// `provider/model`.
    pub name: String,
    pub provider: wb_providers::config::ProviderConfig,
    pub max_output_tokens: Option<u32>,
}

/// Convert between the two crates' config types through their shared serialized form.
fn convert<A: serde::Serialize, B: serde::de::DeserializeOwned>(value: &A) -> Result<B> {
    Ok(serde_json::from_value(serde_json::to_value(value)?)?)
}

/// Resolve `provider/model` (or the configured default) to a client with its capability
/// record, probing on first use.
pub async fn resolve_model(
    home: &Path,
    global: &GlobalConfig,
    spec: Option<&str>,
    force_probe: bool,
) -> Result<ResolvedModel> {
    let spec = spec
        .map(str::to_string)
        .or_else(|| global.defaults.model.clone())
        .ok_or_else(|| anyhow!("no model given and no [defaults] model in config.toml"))?;
    let (provider_name, model_id) = spec
        .split_once('/')
        .ok_or_else(|| anyhow!("model must be written as <provider>/<model id>, got {spec:?}"))?;
    let provider_cfg = global
        .providers
        .iter()
        .find(|p| p.name == provider_name)
        .ok_or_else(|| anyhow!("no provider named {provider_name:?} in config.toml"))?;
    let provider: wb_providers::config::ProviderConfig =
        convert(provider_cfg).context("provider configuration")?;
    let network: wb_providers::config::NetworkConfig =
        convert(&global.network).context("network configuration")?;
    let keys = wb_providers::keys::KeyStores::system("Personal Present");
    let options = ClientOptions {
        network: &network,
        keys: &keys,
        audit: None,
        home: Some(home),
    };
    let client = wb_providers::client_for(&provider, model_id, &options)?;
    let entry = global
        .models
        .iter()
        .find(|m| m.provider == provider_name && m.id == model_id);
    let overrides: Option<wb_providers::config::ModelConfig> = match entry {
        Some(m) => Some(convert(m)?),
        None => None,
    };
    let key = wb_providers::capability_key(provider_name, model_id);
    let caps = capabilities::ensure(
        home,
        &key,
        client.as_ref(),
        overrides.as_ref(),
        None,
        force_probe,
    )
    .await?;
    Ok(ResolvedModel {
        client,
        caps,
        name: spec.clone(),
        provider,
        max_output_tokens: entry.and_then(|m| m.max_output_tokens),
    })
}

/// The project's policy as the agent evaluates it.
pub fn policy_for(project: &ProjectConfig) -> Result<Policy> {
    let cfg: PresentPolicyConfig = convert(&project.policy).context("[policy] in project.toml")?;
    Ok(cfg.to_policy())
}

/// Skills visible to a project: built-in, user, then project (PROTOCOL 11).
pub fn discover_skills(home: &Path, project: &Path) -> SkillSet {
    // Built-in skills ship with the runtime; a development checkout reads them from the
    // repository, and `PRESENT_SKILLS_DIR` names them for an installed app.
    let builtin = std::env::var_os("PRESENT_SKILLS_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| Some(home.join("runtime").join("skills")).filter(|p| p.is_dir()))
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills"));
    let roots = [
        SkillRoot::new(SkillSource::Builtin, builtin),
        SkillRoot::new(SkillSource::User, home.join("skills")),
        SkillRoot::new(
            SkillSource::Project,
            project.join(".present").join("skills"),
        ),
    ];
    let existing: Vec<SkillRoot> = roots.into_iter().filter(|r| r.path.is_dir()).collect();
    let (set, diagnostics) = SkillSet::discover(&existing);
    for d in diagnostics {
        tracing::warn!("skill diagnostic: {d:?}");
    }
    set
}

/// Where a project's conversations live under the Present home.
pub fn transcript_store(home: &Path, project: &Path) -> TranscriptStore {
    let key = crate::util::sha256_hex(project.to_string_lossy().as_bytes());
    TranscriptStore::new(home.join("projects").join(&key[..16]))
}

/// Assemble an agent for a conversation in a workspace.
pub fn build_agent(
    ws: Arc<Workspace>,
    model: &ResolvedModel,
    policy: Policy,
    approver: Arc<dyn Approver>,
    mode: Mode,
    conv_id: &str,
    max_steps: u32,
) -> Result<Agent> {
    let mut config = AgentConfig::new(mode, &model.name, model.caps.clone(), max_steps);
    config.provider = model.provider.name.clone();
    config.provider_location = model.provider.location;
    config.base_url_host = model.provider.base_url_host();
    if model.max_output_tokens.is_some() {
        config.max_tokens = model.max_output_tokens;
    }
    let store = transcript_store(&ws.home, &ws.project.root);
    let mut agent = Agent::new(
        model.client.clone(),
        ws.clone(),
        policy,
        approver,
        config,
        store,
        conv_id,
    );
    agent.state.skills = Some(discover_skills(&ws.home, &ws.project.root));
    agent.audit = Some(Arc::new(AuditLog::new(&ws.home)));
    agent.resume()?;
    Ok(agent)
}
