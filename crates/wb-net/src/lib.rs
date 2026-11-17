//! The network gate.
//!
//! The app may reach the network for three things only: model endpoints, the Python package
//! index, and SSH execution targets. This crate is the one place that happens:
//!
//! - HTTP requests to **model endpoints** are built by [`Gate::request`], which refuses any
//!   destination that is not on the allowlist and follows redirects only to allowlisted hosts.
//!   No other crate constructs an HTTP client (`tools/check_egress.py` enforces this).
//! - **Package installs** and **SSH** run as subprocesses (`uv`, `ssh`, `scp`, `rsync`), which
//!   the gate cannot intercept. Callers must ask [`Gate::authorize_package_index`] or
//!   [`Gate::authorize_ssh`] before they spawn one, and must run every other `uv` invocation
//!   with [`OFFLINE_ENV`] so it cannot reach an index.
//!
//! Every decision, allowed or denied, is appended to the egress ledger (JSONL) when one is
//! configured. If the ledger cannot be written the request is refused: nothing leaves the
//! machine unrecorded.
//!
//! The allowlist is filled from the user's configuration (provider `base_url`s, the package
//! index URL, configured SSH targets), never from model output.
//!
//! The HTTP client ignores proxy settings and uses the bundled web trust roots: endpoints
//! and mirrors are reached directly.

pub mod ledger;
pub mod secrets;

/// Re-exported so other crates can name request and response types without depending on
/// `reqwest` themselves. Clients are built here only.
pub use reqwest;

use ledger::{Ledger, LedgerEntry};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// The default Python package index.
pub const DEFAULT_PACKAGE_INDEX: &str = "https://pypi.org/simple";

/// Environment for every `uv` or `pip` invocation that is not an authorized install: it makes
/// the tool fail rather than contact an index.
pub const OFFLINE_ENV: &[(&str, &str)] = &[("UV_OFFLINE", "1"), ("PIP_NO_INDEX", "1")];

/// What an outbound connection is for. There are no other classes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EgressClass {
    /// Chat, vision, and embedding requests to a configured model provider.
    ModelEndpoint,
    /// Package installation from the configured index, run by the core.
    PackageIndex,
    /// A configured SSH execution target (including a cluster login node).
    Ssh,
}

impl EgressClass {
    pub fn as_str(self) -> &'static str {
        match self {
            EgressClass::ModelEndpoint => "model_endpoint",
            EgressClass::PackageIndex => "package_index",
            EgressClass::Ssh => "ssh",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error(
        "network access to {destination} is not allowed ({class}): it is not a configured {what}"
    )]
    Denied {
        class: &'static str,
        destination: String,
        what: &'static str,
    },
    #[error("`{0}` is not a usable network destination")]
    BadDestination(String),
    #[error("the egress ledger could not be written ({0}); the request was not sent")]
    Ledger(String),
    #[error("building the HTTP client: {0}")]
    Client(String),
}

type Allow = BTreeSet<(EgressClass, String)>;

/// The gate. Cheap to clone; clones share one allowlist, ledger, and HTTP client.
#[derive(Clone)]
pub struct Gate {
    inner: Arc<Inner>,
}

struct Inner {
    allow: Arc<RwLock<Allow>>,
    ledger: RwLock<Option<Ledger>>,
    http: reqwest::Client,
}

impl Gate {
    /// A gate with an empty allowlist. `ledger` is the JSONL file decisions are appended to.
    pub fn new(ledger: Option<PathBuf>) -> Result<Gate, EgressError> {
        let allow: Arc<RwLock<Allow>> = Arc::default();
        let for_redirects = allow.clone();
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("too many redirects");
                }
                let allowed = http_destination(attempt.url().as_str())
                    .map(|d| {
                        for_redirects
                            .read()
                            .map(|a| a.contains(&(EgressClass::ModelEndpoint, d)))
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if allowed {
                    attempt.follow()
                } else {
                    attempt.error("redirect to a host that is not a configured model endpoint")
                }
            }))
            .build()
            .map_err(|e| EgressError::Client(e.to_string()))?;
        Ok(Gate {
            inner: Arc::new(Inner {
                allow,
                ledger: RwLock::new(ledger.map(Ledger::new)),
                http,
            }),
        })
    }

    /// Start (or move) the ledger. Used once at startup, when the app knows its home directory.
    pub fn set_ledger(&self, path: Option<PathBuf>) {
        if let Ok(mut l) = self.inner.ledger.write() {
            *l = path.map(Ledger::new);
        }
    }

    pub fn ledger_path(&self) -> Option<PathBuf> {
        self.inner
            .ledger
            .read()
            .ok()
            .and_then(|l| l.as_ref().map(|l| l.path().to_path_buf()))
    }

    /// Allow HTTP requests to the host and port of a provider's `base_url`.
    pub fn allow_model_endpoint(&self, base_url: &str) -> Result<(), EgressError> {
        let d = http_destination(base_url)?;
        self.insert(EgressClass::ModelEndpoint, d);
        Ok(())
    }

    /// Allow package installs from this index URL.
    pub fn allow_package_index(&self, index_url: &str) -> Result<(), EgressError> {
        let d = http_destination(index_url)?;
        self.insert(EgressClass::PackageIndex, d);
        Ok(())
    }

    /// Allow SSH connections to this host (a name as it is written in the targets config).
    pub fn allow_ssh_host(&self, host: &str) -> Result<(), EgressError> {
        let d = ssh_destination(host)?;
        self.insert(EgressClass::Ssh, d);
        Ok(())
    }

    fn insert(&self, class: EgressClass, destination: String) {
        if let Ok(mut a) = self.inner.allow.write() {
            a.insert((class, destination));
        }
    }

    fn contains(&self, class: EgressClass, destination: &str) -> bool {
        self.inner
            .allow
            .read()
            .map(|a| a.contains(&(class, destination.to_string())))
            .unwrap_or(false)
    }

    /// Decide, record, and report. The single path every authorization takes.
    fn decide(
        &self,
        class: EgressClass,
        destination: String,
        purpose: &str,
        what: &'static str,
    ) -> Result<(), EgressError> {
        let allowed = self.contains(class, &destination);
        if let Ok(l) = self.inner.ledger.read() {
            if let Some(l) = l.as_ref() {
                l.append(&LedgerEntry::now(class, &destination, allowed, purpose))
                    .map_err(|e| EgressError::Ledger(e.to_string()))?;
            }
        }
        if allowed {
            Ok(())
        } else {
            tracing::warn!(class = class.as_str(), %destination, purpose, "egress denied");
            Err(EgressError::Denied {
                class: class.as_str(),
                destination,
                what,
            })
        }
    }

    /// A request to a model endpoint. Fails, and records the refusal, unless the URL's host
    /// and port are an allowed model endpoint. `purpose` is a short label for the ledger
    /// (for example `chat`, `probe`, `embeddings`); it must not contain request content.
    pub fn request(
        &self,
        method: reqwest::Method,
        url: &str,
        purpose: &str,
    ) -> Result<reqwest::RequestBuilder, EgressError> {
        let d = http_destination(url)?;
        self.decide(EgressClass::ModelEndpoint, d, purpose, "model endpoint")?;
        Ok(self.inner.http.request(method, url))
    }

    /// Call before spawning `uv` (or `pip`) for an install. On `Ok`, pass `index_url` to the
    /// tool explicitly; every other invocation must carry [`OFFLINE_ENV`].
    pub fn authorize_package_index(
        &self,
        index_url: &str,
        purpose: &str,
    ) -> Result<(), EgressError> {
        let d = http_destination(index_url)?;
        self.decide(EgressClass::PackageIndex, d, purpose, "package index")
    }

    /// Call before spawning `ssh`, `scp`, or `rsync` to `host`.
    pub fn authorize_ssh(&self, host: &str, purpose: &str) -> Result<(), EgressError> {
        let d = ssh_destination(host)?;
        self.decide(EgressClass::Ssh, d, purpose, "SSH target")
    }

    /// Whether a destination is currently allowed, without recording anything.
    pub fn is_allowed(&self, class: EgressClass, destination: &str) -> bool {
        let d = match class {
            EgressClass::Ssh => ssh_destination(destination),
            _ => http_destination(destination),
        };
        d.map(|d| self.contains(class, &d)).unwrap_or(false)
    }

    /// The allowlist, for display in settings and diagnostics.
    pub fn allowed(&self) -> Vec<(EgressClass, String)> {
        self.inner
            .allow
            .read()
            .map(|a| a.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// `host:port` of an http(s) URL, lowercased, with the scheme's default port filled in.
pub fn http_destination(url: &str) -> Result<String, EgressError> {
    let bad = || EgressError::BadDestination(url.to_string());
    let u = reqwest::Url::parse(url.trim()).map_err(|_| bad())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(bad());
    }
    let host = u.host_str().ok_or_else(bad)?.to_ascii_lowercase();
    let port = u.port_or_known_default().ok_or_else(bad)?;
    Ok(format!("{host}:{port}"))
}

/// The host part of an SSH destination (`user@host` or `host`), lowercased. Names starting
/// with `-` are rejected so a host can never be read as an option.
pub fn ssh_destination(host: &str) -> Result<String, EgressError> {
    let h = host.trim();
    let h = h.rsplit_once('@').map(|(_, h)| h).unwrap_or(h);
    if h.is_empty() || h.starts_with('-') || h.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(EgressError::BadDestination(host.to_string()));
    }
    Ok(h.to_ascii_lowercase())
}

static GATE: OnceLock<Gate> = OnceLock::new();

/// The process-wide gate. It starts with an empty allowlist and no ledger; the app calls
/// [`init`] at startup and registers destinations as it reads the user's configuration.
pub fn gate() -> &'static Gate {
    GATE.get_or_init(|| Gate::new(None).expect("the HTTP client builds with static settings"))
}

/// Point the process-wide gate's ledger at `<home>/egress.jsonl`.
pub fn init(home: &Path) -> &'static Gate {
    let g = gate();
    g.set_ledger(Some(home.join(ledger::LEDGER_FILE)));
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_are_normalised() {
        assert_eq!(
            http_destination("https://API.Example.com/v1").unwrap(),
            "api.example.com:443"
        );
        assert_eq!(
            http_destination("http://127.0.0.1:8765/v1").unwrap(),
            "127.0.0.1:8765"
        );
        assert_eq!(
            http_destination(" http://mirror.corp/simple ").unwrap(),
            "mirror.corp:80"
        );
        assert!(http_destination("ftp://example.com").is_err());
        assert!(http_destination("not a url").is_err());
        assert_eq!(ssh_destination("me@Lab-GPU").unwrap(), "lab-gpu");
        assert!(ssh_destination("-oProxyCommand=x").is_err());
        assert!(ssh_destination("a b").is_err());
        assert!(ssh_destination("").is_err());
    }

    #[test]
    fn nothing_is_allowed_until_configured() {
        let g = Gate::new(None).unwrap();
        assert!(g
            .request(
                reqwest::Method::POST,
                "https://api.example.com/v1/chat",
                "chat"
            )
            .is_err());
        assert!(g
            .authorize_package_index(DEFAULT_PACKAGE_INDEX, "uv add")
            .is_err());
        assert!(g.authorize_ssh("lab-gpu", "run").is_err());
    }

    #[test]
    fn classes_do_not_leak_into_each_other() {
        let g = Gate::new(None).unwrap();
        g.allow_package_index("https://pypi.org/simple").unwrap();
        g.allow_ssh_host("pypi.org").unwrap();
        // An index host is not a model endpoint, and an SSH host is not either.
        assert!(g
            .request(reqwest::Method::GET, "https://pypi.org/x", "chat")
            .is_err());
        g.allow_model_endpoint("https://api.example.com/v1")
            .unwrap();
        assert!(g
            .request(
                reqwest::Method::POST,
                "https://api.example.com/v1/messages",
                "chat"
            )
            .is_ok());
        // Same host, other port or scheme default: a different destination.
        assert!(g
            .request(
                reqwest::Method::POST,
                "https://api.example.com:8443/v1",
                "chat"
            )
            .is_err());
        assert!(g
            .request(reqwest::Method::POST, "http://api.example.com/v1", "chat")
            .is_err());
        assert!(g
            .authorize_package_index("https://api.example.com/simple", "uv add")
            .is_err());
        assert!(g
            .authorize_package_index("https://pypi.org/simple", "uv add")
            .is_ok());
        assert!(g.authorize_ssh("user@pypi.org", "sync").is_ok());
        assert!(g.is_allowed(EgressClass::Ssh, "pypi.org"));
        assert!(!g.is_allowed(EgressClass::Ssh, "api.example.com"));
    }

    #[test]
    fn every_decision_is_recorded_and_an_unwritable_ledger_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("egress.jsonl");
        let g = Gate::new(Some(path.clone())).unwrap();
        g.allow_model_endpoint("http://127.0.0.1:9/v1").unwrap();
        let _ = g
            .request(reqwest::Method::POST, "http://127.0.0.1:9/v1/chat", "chat")
            .unwrap();
        assert!(g.authorize_ssh("elsewhere", "run").is_err());
        let lines = ledger::read(&path).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            (lines[0].class, lines[0].allowed),
            (EgressClass::ModelEndpoint, true)
        );
        assert_eq!(lines[0].destination, "127.0.0.1:9");
        assert_eq!(lines[0].purpose, "chat");
        assert_eq!(
            (lines[1].class, lines[1].allowed),
            (EgressClass::Ssh, false)
        );

        // A ledger that cannot be written refuses even an allowed destination.
        let g = Gate::new(Some(
            dir.path()
                .join("missing-dir")
                .join("no")
                .join("egress.jsonl"),
        ))
        .unwrap();
        std::fs::write(dir.path().join("missing-dir"), b"a file, not a directory").unwrap();
        g.allow_model_endpoint("http://127.0.0.1:9/v1").unwrap();
        let err = g
            .request(reqwest::Method::POST, "http://127.0.0.1:9/v1/chat", "chat")
            .unwrap_err();
        assert!(matches!(err, EgressError::Ledger(_)), "{err}");
    }
}
