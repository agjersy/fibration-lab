// Ported from Personal Analyst `crates/protocol/src/config.rs` (provider and model sections)
// and `crates/core/src/config.rs` (`api_key`) at commit f229e4d.
// Adapted: `api_key_env` is replaced by `auth` + `key`; adds `headers`, `location`, and the
// `[network]` section (Present PROTOCOL 4.1).
//! Provider, model, and network configuration as written in the global `config.toml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderKind {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    #[serde(rename = "anthropic")]
    Anthropic,
}

/// How the key is presented to the endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuthKind {
    /// `x-api-key: <key>`.
    #[serde(rename = "x-api-key")]
    XApiKey,
    /// `Authorization: Bearer <key>`.
    #[serde(rename = "bearer")]
    Bearer,
    /// No credential header.
    #[serde(rename = "none")]
    None,
}

/// Whether the endpoint is inside the organization's network. Shown to the user and written
/// to the audit log; it does not change request handling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Location {
    Internal,
    /// The default: an endpoint not declared internal is treated as external.
    #[default]
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Defaults by kind when absent: see `effective_auth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthKind>,
    /// `keychain:<entry>` or `env:<VAR>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Extra headers sent on every request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub location: Location,
}

impl ProviderConfig {
    /// The effective base URL: the configured one or the provider kind's default.
    pub fn effective_base_url(&self) -> String {
        match (&self.base_url, self.kind) {
            (Some(u), _) => u.trim_end_matches('/').to_string(),
            (None, ProviderKind::Anthropic) => "https://api.anthropic.com".to_string(),
            (None, ProviderKind::OpenAiCompatible) => "http://localhost:11434/v1".to_string(),
        }
    }

    /// The configured auth scheme, or the default for the wire kind: `x-api-key` for
    /// `anthropic`; for `openai-compatible`, `bearer` when a key is configured and `none`
    /// otherwise (local servers need no key).
    pub fn effective_auth(&self) -> AuthKind {
        match (self.auth, self.kind) {
            (Some(a), _) => a,
            (None, ProviderKind::Anthropic) => AuthKind::XApiKey,
            (None, ProviderKind::OpenAiCompatible) if self.key.is_some() => AuthKind::Bearer,
            (None, ProviderKind::OpenAiCompatible) => AuthKind::None,
        }
    }

    /// Host part of the effective base URL (for display and the audit log); empty when the
    /// URL does not parse.
    pub fn base_url_host(&self) -> String {
        reqwest::Url::parse(&self.effective_base_url())
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolSupport {
    Native,
    TextProtocol,
    None,
}

/// A `[[models]]` entry: per-model overrides of the probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelConfig {
    pub provider: String,
    /// Sent verbatim; may be a gateway alias.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// `proxy = "system" | "none" | "<url>"`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ProxySetting {
    /// The operating system's proxy settings and the usual `HTTPS_PROXY`/`NO_PROXY` variables.
    #[default]
    System,
    /// Direct connections; environment and system proxies are ignored.
    None,
    /// All requests go through this proxy.
    Url(String),
}

impl From<String> for ProxySetting {
    fn from(s: String) -> Self {
        match s.trim() {
            "" | "system" => ProxySetting::System,
            "none" => ProxySetting::None,
            url => ProxySetting::Url(url.to_string()),
        }
    }
}

impl From<ProxySetting> for String {
    fn from(p: ProxySetting) -> String {
        match p {
            ProxySetting::System => "system".into(),
            ProxySetting::None => "none".into(),
            ProxySetting::Url(u) => u,
        }
    }
}

/// `ca_bundle = "system" | "<path to a PEM file>"`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum CaBundle {
    /// The operating system's trust store only.
    #[default]
    System,
    /// The operating system's trust store plus every certificate in this PEM file.
    Path(PathBuf),
}

impl From<String> for CaBundle {
    fn from(s: String) -> Self {
        match s.trim() {
            "" | "system" => CaBundle::System,
            p => CaBundle::Path(PathBuf::from(p)),
        }
    }
}

impl From<CaBundle> for String {
    fn from(c: CaBundle) -> String {
        match c {
            CaBundle::System => "system".into(),
            CaBundle::Path(p) => p.to_string_lossy().into_owned(),
        }
    }
}

/// The `[network]` section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NetworkConfig {
    #[serde(default)]
    pub proxy: ProxySetting,
    #[serde(default)]
    pub ca_bundle: CaBundle,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_parses_with_defaults() {
        let p: ProviderConfig = serde_json::from_value(json!({
            "name": "gateway", "kind": "anthropic", "base_url": "https://llm-gateway.corp.example/",
            "auth": "bearer", "key": "keychain:gateway",
            "headers": {"X-Team": "strategy"}, "location": "internal"
        }))
        .unwrap();
        assert_eq!(p.effective_auth(), AuthKind::Bearer);
        assert_eq!(p.location, Location::Internal);
        assert_eq!(p.effective_base_url(), "https://llm-gateway.corp.example");
        assert_eq!(p.base_url_host(), "llm-gateway.corp.example");
        assert_eq!(p.headers["X-Team"], "strategy");

        let a: ProviderConfig =
            serde_json::from_value(json!({"name": "a", "kind": "anthropic"})).unwrap();
        assert_eq!(a.effective_auth(), AuthKind::XApiKey);
        assert_eq!(a.location, Location::External);
        assert_eq!(a.effective_base_url(), "https://api.anthropic.com");

        let o: ProviderConfig =
            serde_json::from_value(json!({"name": "o", "kind": "openai-compatible"})).unwrap();
        assert_eq!(o.effective_auth(), AuthKind::None);
        let o: ProviderConfig = serde_json::from_value(
            json!({"name": "o", "kind": "openai-compatible", "key": "env:K"}),
        )
        .unwrap();
        assert_eq!(o.effective_auth(), AuthKind::Bearer);
        let o: ProviderConfig = serde_json::from_value(
            json!({"name": "o", "kind": "openai-compatible", "auth": "none"}),
        )
        .unwrap();
        assert_eq!(o.effective_auth(), AuthKind::None);
    }

    #[test]
    fn network_settings_parse() {
        let n: NetworkConfig = serde_json::from_value(json!({})).unwrap();
        assert_eq!(n, NetworkConfig::default());
        let n: NetworkConfig =
            serde_json::from_value(json!({"proxy": "none", "ca_bundle": "system"})).unwrap();
        assert_eq!(n.proxy, ProxySetting::None);
        assert_eq!(n.ca_bundle, CaBundle::System);
        let n: NetworkConfig = serde_json::from_value(
            json!({"proxy": "http://proxy.corp:8080", "ca_bundle": "/etc/corp.pem"}),
        )
        .unwrap();
        assert_eq!(n.proxy, ProxySetting::Url("http://proxy.corp:8080".into()));
        assert_eq!(n.ca_bundle, CaBundle::Path("/etc/corp.pem".into()));
        assert_eq!(
            serde_json::to_value(&n).unwrap(),
            json!({"proxy": "http://proxy.corp:8080", "ca_bundle": "/etc/corp.pem"})
        );
    }
}
