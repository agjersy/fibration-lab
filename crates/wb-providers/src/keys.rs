//! API key resolution (Present PROTOCOL 4.1): `key = "env:<VAR>"`.
//!
//! A key is named by an environment variable. It is read from the process environment first
//! and then from the `.env` files the caller names: the app home's (`<home>/.env`, where keys
//! entered in settings are written with mode 0600) and optionally a project's. There is no
//! other key storage; `keychain:<entry>` references from older configurations are refused
//! with an explanation.

use crate::config::{AuthKind, ProviderConfig};
use std::path::{Path, PathBuf};
use wb_net::secrets::{self, EnvFile};

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error(
        "key reference `{0}` must be `env:<VAR>`, where <VAR> is an environment variable name"
    )]
    BadReference(String),
    #[error(
        "key reference `keychain:{0}` is no longer supported: the OS keychain is not used. \
         Set `key = \"env:<VAR>\"` and put the value in the environment or in the `.env` file \
         in the app home"
    )]
    KeychainRemoved(String),
    #[error("this key store has no `.env` file to write to")]
    ReadOnly,
    #[error("could not write {path}: {message}")]
    Write { path: String, message: String },
    #[error(
        "provider `{provider}` needs a key ({reference}), which is not set in the environment \
         or the `.env` file"
    )]
    Missing { provider: String, reference: String },
    #[error("provider `{0}` uses auth that needs a key but has no `key` in its configuration")]
    NotConfigured(String),
}

/// A parsed `key` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRef {
    /// `env:<VAR>`.
    Env(String),
}

impl KeyRef {
    pub fn parse(s: &str) -> Result<KeyRef, KeyError> {
        match s.split_once(':') {
            Some(("env", v)) if secrets::valid_name(v) => Ok(KeyRef::Env(v.to_string())),
            Some(("keychain", e)) => Err(KeyError::KeychainRemoved(e.to_string())),
            _ => Err(KeyError::BadReference(s.to_string())),
        }
    }

    /// The variable name.
    pub fn var(&self) -> &str {
        match self {
            KeyRef::Env(v) => v,
        }
    }
}

/// Where keys are looked up: the process environment, then these `.env` files in order.
#[derive(Debug, Clone, Default)]
pub struct KeyStores {
    files: Vec<PathBuf>,
}

impl KeyStores {
    /// The environment plus the given `.env` files. The first file is the one `set` and
    /// `remove` write (the app home's).
    pub fn new(files: impl IntoIterator<Item = PathBuf>) -> Self {
        KeyStores {
            files: files.into_iter().collect(),
        }
    }

    /// The environment plus `<home>/.env`.
    pub fn for_home(home: &Path) -> Self {
        Self::new([home.join(secrets::ENV_FILE)])
    }

    /// Also read a project's `.env` (after the home file). A file git tracks is ignored.
    pub fn with_project_env(mut self, path: PathBuf) -> Self {
        self.files.push(path);
        self
    }

    /// The `.env` files consulted after the environment.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Resolve one reference; `None` when the variable is set nowhere.
    pub fn get(&self, reference: &KeyRef) -> Option<String> {
        let files: Vec<&Path> = self.files.iter().map(PathBuf::as_path).collect();
        secrets::lookup(reference.var(), &files).map(|(value, _)| value)
    }

    fn home_file(&self) -> Result<EnvFile, KeyError> {
        self.files
            .first()
            .map(EnvFile::new)
            .ok_or(KeyError::ReadOnly)
    }

    /// Write `VAR=secret` to the first `.env` file (created with mode 0600). A value in the
    /// process environment still takes precedence on lookup.
    pub fn set(&self, reference: &KeyRef, secret: &str) -> Result<(), KeyError> {
        let file = self.home_file()?;
        file.set(reference.var(), secret)
            .map_err(|e| write_error(&file, e))
    }

    /// Remove the variable from the first `.env` file.
    pub fn remove(&self, reference: &KeyRef) -> Result<(), KeyError> {
        let file = self.home_file()?;
        file.remove(reference.var())
            .map_err(|e| write_error(&file, e))
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
        match self.get(&KeyRef::parse(reference)?) {
            Some(k) => Ok(Some(k)),
            None => Err(KeyError::Missing {
                provider: provider.name.clone(),
                reference: reference.clone(),
            }),
        }
    }
}

fn write_error(file: &EnvFile, e: std::io::Error) -> KeyError {
    KeyError::Write {
        path: file.path().display().to_string(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
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

    // Variable names here are unique to this file and never set in the environment.
    const VAR: &str = "WB_PROVIDERS_KEYS_TEST_GATEWAY_KEY";

    fn stores() -> (tempfile::TempDir, KeyStores) {
        let home = tempfile::tempdir().unwrap();
        let s = KeyStores::for_home(home.path());
        s.set(&KeyRef::Env(VAR.into()), "file-secret").unwrap();
        (home, s)
    }

    #[test]
    fn key_reference_parsing() {
        assert_eq!(KeyRef::parse("env:X").unwrap(), KeyRef::Env("X".into()));
        assert!(matches!(
            KeyRef::parse("sk-raw-secret"),
            Err(KeyError::BadReference(_))
        ));
        assert!(KeyRef::parse("env:").is_err());
        assert!(KeyRef::parse("env:NOT A NAME").is_err());
        assert!(KeyRef::parse("file:/x").is_err());
    }

    #[test]
    fn keychain_references_are_refused_with_the_way_out() {
        let err = KeyRef::parse("keychain:gateway").unwrap_err();
        assert!(matches!(err, KeyError::KeychainRemoved(_)));
        let text = err.to_string();
        assert!(
            text.contains("env:<VAR>") && text.contains(".env"),
            "{text}"
        );
        let (_home, s) = stores();
        let p = provider(ProviderKind::Anthropic, None, Some("keychain:gateway"));
        assert!(matches!(s.resolve(&p), Err(KeyError::KeychainRemoved(_))));
    }

    #[test]
    fn resolves_from_the_env_file_and_the_environment_wins() {
        let (home, s) = stores();
        let p = provider(ProviderKind::Anthropic, None, Some(&format!("env:{VAR}")));
        assert_eq!(s.resolve(&p).unwrap().as_deref(), Some("file-secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.path().join(".env"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // PATH is set in every test environment; the file's value is shadowed by it.
        s.set(&KeyRef::Env("PATH".into()), "from-file").unwrap();
        assert_ne!(
            s.get(&KeyRef::Env("PATH".into())).as_deref(),
            Some("from-file")
        );

        // A second (project) file is read after the home file.
        let project = home.path().join("project.env");
        std::fs::write(&project, format!("{VAR}=project\n{VAR}_2=project-only\n")).unwrap();
        let s = s.with_project_env(project);
        assert_eq!(
            s.get(&KeyRef::Env(VAR.into())).as_deref(),
            Some("file-secret")
        );
        assert_eq!(
            s.get(&KeyRef::Env(format!("{VAR}_2"))).as_deref(),
            Some("project-only")
        );

        s.remove(&KeyRef::Env(VAR.into())).unwrap();
        assert_eq!(s.get(&KeyRef::Env(VAR.into())).as_deref(), Some("project"));
    }

    #[test]
    fn auth_none_needs_no_key_and_missing_keys_are_errors() {
        let (_home, s) = stores();
        let p = provider(ProviderKind::OpenAiCompatible, None, None);
        assert_eq!(s.resolve(&p).unwrap(), None);
        // auth = none wins even when a key is configured.
        let p = provider(
            ProviderKind::OpenAiCompatible,
            Some(AuthKind::None),
            Some(&format!("env:{VAR}")),
        );
        assert_eq!(s.resolve(&p).unwrap(), None);
        let p = provider(ProviderKind::Anthropic, None, None);
        assert!(matches!(s.resolve(&p), Err(KeyError::NotConfigured(_))));
        let p = provider(
            ProviderKind::Anthropic,
            None,
            Some("env:WB_PROVIDERS_KEYS_TEST_UNSET"),
        );
        assert!(matches!(s.resolve(&p), Err(KeyError::Missing { .. })));
    }

    #[test]
    fn a_store_without_files_reads_the_environment_and_cannot_write() {
        let s = KeyStores::default();
        assert!(s.get(&KeyRef::Env("PATH".into())).is_some());
        assert_eq!(
            s.get(&KeyRef::Env("WB_PROVIDERS_KEYS_TEST_UNSET".into())),
            None
        );
        assert!(matches!(
            s.set(&KeyRef::Env("X".into()), "y"),
            Err(KeyError::ReadOnly)
        ));
    }
}
