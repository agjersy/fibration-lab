// Ported from Personal Analyst `crates/protocol/src/config.rs` (provider and model sections)
// and `crates/core/src/config.rs` (`api_key`) at commit f229e4d.
// Adapted: `api_key_env` is replaced by `auth` + `key`; adds `headers` and `location`
// (Present PROTOCOL 4.1).
//! Provider and model configuration as written in the global `config.toml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// `env:<VAR>`: the variable is read from the process environment, then from `.env`.
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
        wb_net::reqwest::Url::parse(&self.effective_base_url())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_parses_with_defaults() {
        let p: ProviderConfig = serde_json::from_value(json!({
            "name": "gateway", "kind": "anthropic", "base_url": "https://llm-gateway.corp.example/",
            "auth": "bearer", "key": "env:GATEWAY_KEY",
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
}
