//! API key resolution (Present PROTOCOL 4.1): `key = "keychain:<entry>" | "env:<VAR>"`.
//! New in this crate (Analyst read `api_key_env` only).

use crate::config::{AuthKind, ProviderConfig};

/// Keychain service name under which entries are stored when none is given.
pub const DEFAULT_KEYCHAIN_SERVICE: &str = "present";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("key reference `{0}` must be `keychain:<entry>` or `env:<VAR>`")]
    BadReference(String),
    #[error("this key store is read-only")]
    ReadOnly,
    #[error("no OS keychain is available in this build; use `env:<VAR>`")]
    NoKeychain,
    #[error("keychain error: {0}")]
    Backend(String),
    #[error("provider `{provider}` needs a key ({reference}), which is not set")]
    Missing { provider: String, reference: String },
    #[error("provider `{0}` uses auth that needs a key but has no `key` in its configuration")]
    NotConfigured(String),
}

/// A parsed `key` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRef {
    Keychain(String),
    Env(String),
}

impl KeyRef {
    pub fn parse(s: &str) -> Result<KeyRef, KeyError> {
        match s.split_once(':') {
            Some(("keychain", e)) if !e.is_empty() => Ok(KeyRef::Keychain(e.to_string())),
            Some(("env", v)) if !v.is_empty() => Ok(KeyRef::Env(v.to_string())),
            _ => Err(KeyError::BadReference(s.to_string())),
        }
    }
}

/// A place secrets are read from (and, where it makes sense, written to).
pub trait KeyStore: Send + Sync {
    /// The secret stored under `entry`; `Ok(None)` when there is none.
    fn get(&self, entry: &str) -> Result<Option<String>, KeyError>;
    fn set(&self, entry: &str, secret: &str) -> Result<(), KeyError>;
    fn delete(&self, entry: &str) -> Result<(), KeyError>;
}

/// Reads keys from environment variables (`entry` is the variable name). For development
/// and CI; read-only.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvKeyStore;

impl KeyStore for EnvKeyStore {
    fn get(&self, entry: &str) -> Result<Option<String>, KeyError> {
        Ok(std::env::var(entry).ok().filter(|v| !v.is_empty()))
    }
    fn set(&self, _entry: &str, _secret: &str) -> Result<(), KeyError> {
        Err(KeyError::ReadOnly)
    }
    fn delete(&self, _entry: &str) -> Result<(), KeyError> {
        Err(KeyError::ReadOnly)
    }
}

/// A store with no keychain behind it; every operation fails with `NoKeychain`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoKeychain;

impl KeyStore for NoKeychain {
    fn get(&self, _entry: &str) -> Result<Option<String>, KeyError> {
        Err(KeyError::NoKeychain)
    }
    fn set(&self, _entry: &str, _secret: &str) -> Result<(), KeyError> {
        Err(KeyError::NoKeychain)
    }
    fn delete(&self, _entry: &str) -> Result<(), KeyError> {
        Err(KeyError::NoKeychain)
    }
}

/// The OS keychain through the `keyring` crate: Keychain on macOS, Credential Manager on
/// Windows. On other platforms no native back end is compiled in and every operation fails
/// with `NoKeychain` (rather than silently using `keyring`'s in-memory mock store).
#[cfg(feature = "keychain")]
#[derive(Debug, Clone)]
pub struct KeychainKeyStore {
    service: String,
}

#[cfg(feature = "keychain")]
impl KeychainKeyStore {
    pub fn new(service: impl Into<String>) -> Self {
        KeychainKeyStore {
            service: service.into(),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn entry(&self, entry: &str) -> Result<keyring::Entry, KeyError> {
        keyring::Entry::new(&self.service, entry).map_err(|e| KeyError::Backend(e.to_string()))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn entry(&self, _entry: &str) -> Result<keyring::Entry, KeyError> {
        let _ = &self.service;
        Err(KeyError::NoKeychain)
    }
}

#[cfg(feature = "keychain")]
impl Default for KeychainKeyStore {
    fn default() -> Self {
        Self::new(DEFAULT_KEYCHAIN_SERVICE)
    }
}

#[cfg(feature = "keychain")]
impl KeyStore for KeychainKeyStore {
    fn get(&self, entry: &str) -> Result<Option<String>, KeyError> {
        match self.entry(entry)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeyError::Backend(e.to_string())),
        }
    }
    fn set(&self, entry: &str, secret: &str) -> Result<(), KeyError> {
        self.entry(entry)?
            .set_password(secret)
            .map_err(|e| KeyError::Backend(e.to_string()))
    }
    fn delete(&self, entry: &str) -> Result<(), KeyError> {
        match self.entry(entry)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeyError::Backend(e.to_string())),
        }
    }
}

/// The stores a `key` reference is resolved against.
pub struct KeyStores {
    pub env: Box<dyn KeyStore>,
    pub keychain: Box<dyn KeyStore>,
}

impl KeyStores {
    /// Environment variables plus the OS keychain when the `keychain` feature is on (service
    /// name `service`), else a keychain that always reports `NoKeychain`.
    pub fn system(service: &str) -> Self {
        #[cfg(feature = "keychain")]
        let keychain: Box<dyn KeyStore> = Box::new(KeychainKeyStore::new(service));
        #[cfg(not(feature = "keychain"))]
        let keychain: Box<dyn KeyStore> = {
            let _ = service;
            Box::new(NoKeychain)
        };
        KeyStores {
            env: Box::new(EnvKeyStore),
            keychain,
        }
    }

    /// Resolve one reference; `Ok(None)` when the entry or variable is not set.
    pub fn get(&self, reference: &KeyRef) -> Result<Option<String>, KeyError> {
        match reference {
            KeyRef::Env(var) => self.env.get(var),
            KeyRef::Keychain(entry) => self.keychain.get(entry),
        }
    }

    /// The key for a provider. `Ok(None)` when its effective auth is `none`; an error when a
    /// key is needed but not configured or not set.
    pub fn resolve(&self, provider: &ProviderConfig) -> Result<Option<String>, KeyError> {
        if provider.effective_auth() == AuthKind::None {
            return Ok(None);
        }
        let Some(reference) = &provider.key else {
            return Err(KeyError::NotConfigured(provider.name.clone()));
        };
        match self.get(&KeyRef::parse(reference)?)? {
            Some(k) => Ok(Some(k)),
            None => Err(KeyError::Missing {
                provider: provider.name.clone(),
                reference: reference.clone(),
            }),
        }
    }
}

impl Default for KeyStores {
    fn default() -> Self {
        Self::system(DEFAULT_KEYCHAIN_SERVICE)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// In-memory store for tests.
    #[derive(Default)]
    pub struct MemoryKeyStore(pub Mutex<BTreeMap<String, String>>);

    impl KeyStore for MemoryKeyStore {
        fn get(&self, entry: &str) -> Result<Option<String>, KeyError> {
            Ok(self.0.lock().unwrap().get(entry).cloned())
        }
        fn set(&self, entry: &str, secret: &str) -> Result<(), KeyError> {
            self.0.lock().unwrap().insert(entry.into(), secret.into());
            Ok(())
        }
        fn delete(&self, entry: &str) -> Result<(), KeyError> {
            self.0.lock().unwrap().remove(entry);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MemoryKeyStore;
    use super::*;
    use crate::config::ProviderKind;

    fn provider(kind: ProviderKind, auth: Option<AuthKind>, key: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            name: "p".into(),
            kind,
            base_url: None,
            auth,
            key: key.map(str::to_string),
            headers: Default::default(),
            location: Default::default(),
        }
    }

    fn stores() -> KeyStores {
        let env = MemoryKeyStore::default();
        env.set("GATEWAY_KEY", "env-secret").unwrap();
        let keychain = MemoryKeyStore::default();
        keychain.set("gateway", "kc-secret").unwrap();
        KeyStores {
            env: Box::new(env),
            keychain: Box::new(keychain),
        }
    }

    #[test]
    fn key_reference_parsing() {
        assert_eq!(
            KeyRef::parse("keychain:gateway").unwrap(),
            KeyRef::Keychain("gateway".into())
        );
        assert_eq!(KeyRef::parse("env:X").unwrap(), KeyRef::Env("X".into()));
        assert!(KeyRef::parse("sk-raw-secret").is_err());
        assert!(KeyRef::parse("env:").is_err());
        assert!(KeyRef::parse("file:/x").is_err());
    }

    #[test]
    fn resolves_through_the_named_store() {
        let s = stores();
        let p = provider(ProviderKind::Anthropic, None, Some("keychain:gateway"));
        assert_eq!(s.resolve(&p).unwrap().as_deref(), Some("kc-secret"));
        let p = provider(ProviderKind::Anthropic, None, Some("env:GATEWAY_KEY"));
        assert_eq!(s.resolve(&p).unwrap().as_deref(), Some("env-secret"));
    }

    #[test]
    fn auth_none_needs_no_key_and_missing_keys_are_errors() {
        let s = stores();
        let p = provider(ProviderKind::OpenAiCompatible, None, None);
        assert_eq!(s.resolve(&p).unwrap(), None);
        // auth = none wins even when a key is configured.
        let p = provider(
            ProviderKind::OpenAiCompatible,
            Some(AuthKind::None),
            Some("env:GATEWAY_KEY"),
        );
        assert_eq!(s.resolve(&p).unwrap(), None);
        let p = provider(ProviderKind::Anthropic, None, None);
        assert!(matches!(s.resolve(&p), Err(KeyError::NotConfigured(_))));
        let p = provider(ProviderKind::Anthropic, None, Some("env:UNSET_VAR"));
        assert!(matches!(s.resolve(&p), Err(KeyError::Missing { .. })));
    }

    #[test]
    fn env_store_reads_the_environment_and_is_read_only() {
        // PATH is set in every test environment; no variable is written here.
        assert!(EnvKeyStore.get("PATH").unwrap().is_some());
        assert_eq!(
            EnvKeyStore.get("WB_PROVIDERS_SURELY_UNSET_VAR").unwrap(),
            None
        );
        assert!(matches!(EnvKeyStore.set("X", "y"), Err(KeyError::ReadOnly)));
    }

    #[test]
    fn system_stores_without_a_keychain_report_it() {
        let s = KeyStores::system("present-test");
        if cfg!(not(feature = "keychain")) || cfg!(target_os = "linux") {
            assert!(matches!(
                s.get(&KeyRef::Keychain("x".into())),
                Err(KeyError::NoKeychain)
            ));
        }
    }
}
