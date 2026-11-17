//! HTTP client construction from the `[network]` section (Present PROTOCOL 4.1, 6.1):
//! proxy selection and extra trust roots. New in this crate.
//!
//! TLS is rustls with the operating system's root store loaded through
//! `rustls-native-certs` (reqwest's `rustls-tls-native-roots`); a configured PEM bundle is
//! added to those roots, never replacing them.

use crate::config::{CaBundle, NetworkConfig, ProxySetting};
use anyhow::{Context, Result};

/// Build the client every adapter request goes through.
pub fn build_http_client(network: &NetworkConfig) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder();
    b = match &network.proxy {
        // reqwest's default: HTTP(S)_PROXY / NO_PROXY, then the OS settings (registry on
        // Windows, System Configuration on macOS).
        ProxySetting::System => b,
        ProxySetting::None => b.no_proxy(),
        ProxySetting::Url(url) => b.proxy(
            reqwest::Proxy::all(url.as_str())
                .with_context(|| format!("invalid [network] proxy `{url}`"))?,
        ),
    };
    if let CaBundle::Path(path) = &network.ca_bundle {
        let pem = std::fs::read(path)
            .with_context(|| format!("reading [network] ca_bundle {}", path.display()))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .with_context(|| format!("parsing [network] ca_bundle {}", path.display()))?;
        if certs.is_empty() {
            anyhow::bail!(
                "[network] ca_bundle {} contains no certificates",
                path.display()
            );
        }
        for c in certs {
            b = b.add_root_certificate(c);
        }
    }
    b.build().context("building the HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_settings_are_reported() {
        let n = NetworkConfig {
            proxy: ProxySetting::Url("not a url".into()),
            ca_bundle: CaBundle::System,
        };
        assert!(build_http_client(&n)
            .unwrap_err()
            .to_string()
            .contains("proxy"));

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.pem");
        let n = NetworkConfig {
            proxy: ProxySetting::None,
            ca_bundle: CaBundle::Path(missing),
        };
        assert!(build_http_client(&n)
            .unwrap_err()
            .to_string()
            .contains("ca_bundle"));

        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        let n = NetworkConfig {
            proxy: ProxySetting::None,
            ca_bundle: CaBundle::Path(empty),
        };
        assert!(build_http_client(&n).is_err());
    }

    #[test]
    fn defaults_build() {
        build_http_client(&NetworkConfig::default()).unwrap();
        build_http_client(&NetworkConfig {
            proxy: ProxySetting::None,
            ca_bundle: CaBundle::System,
        })
        .unwrap();
    }
}
