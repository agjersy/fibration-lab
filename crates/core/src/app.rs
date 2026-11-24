//! Start-up wiring shared by the three front ends: the desktop shell, `analyst-serve`, and the
//! `analyst` CLI. [`App::start`] creates the home, migrates an old `~/.present` once, sets up the
//! network gate's ledger, and loads `config.toml`; from there the shell and the bridge build the
//! [`Workbench`] with [`App::workbench`], and the CLI, which talks to one kernel and one model
//! without a workbench, uses [`App::kernel`], [`App::ensure_local_tooling`] and [`App::model`].
//! The session runtime builds its kernel transport with the same [`kernel_transport`].

use crate::config;
use crate::kernel::{KernelArgs, KernelClient, KernelTransport, LocalStdio, SshStdio};
use crate::migrate::MigrationReport;
use crate::service::{Workbench, WorkbenchOptions};
use crate::target::{self, Target};
use analyst_protocol::config::{GlobalConfig, ProjectConfig};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wb_providers::model::ModelCapabilities;
use wb_providers::{capabilities, ModelClient};

/// What a front end passes to [`App::start`].
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// Kernel sandbox kind (`auto`, `bwrap`, `sandbox-exec`, `limits`, `none`); `None` lets the
    /// kernel choose (`auto` locally, the target's setting remotely).
    pub sandbox: Option<String>,
}

impl StartOptions {
    /// From the environment: `ANALYST_SANDBOX` overrides the kernel sandbox kind.
    pub fn from_env() -> StartOptions {
        StartOptions {
            sandbox: std::env::var("ANALYST_SANDBOX")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }
}

/// The started application: its home and global configuration.
pub struct App {
    pub home: PathBuf,
    pub global: GlobalConfig,
    pub sandbox: Option<String>,
    /// What the first start migrated from `~/.present` (already logged).
    pub migrated: Option<MigrationReport>,
    /// Why migrating `~/.present` failed, if it did (already logged; start-up goes on).
    pub migration_error: Option<String>,
}

impl App {
    /// Create `home`, migrate the deck workbench's old home (`~/.present`) once, point the
    /// network gate's ledger at `<home>/egress.jsonl` (loading the config then registers the
    /// destinations it names), and load `config.toml`.
    pub fn start(home: PathBuf, opts: StartOptions) -> Result<App> {
        std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
        let (migrated, migration_error) = match crate::migrate::migrate_legacy_home(&home) {
            Ok(Some(r)) => {
                tracing::info!(
                    "migrated {} into {}: {} provider(s), {} file(s), {} key(s)",
                    r.from.display(),
                    home.display(),
                    r.providers.len(),
                    r.files.len(),
                    r.keys.len()
                );
                (Some(r), None)
            }
            Ok(None) => (None, None),
            Err(e) => {
                tracing::warn!("migrating ~/.present failed: {e:#}");
                (None, Some(format!("{e:#}")))
            }
        };
        wb_net::init(&home);
        let global = config::load_global(&home)?;
        Ok(App {
            home,
            global,
            sandbox: opts.sandbox,
            migrated,
            migration_error,
        })
    }

    /// [`App::start`] with `$ANALYST_HOME` (default `~/.analyst`) and [`StartOptions::from_env`].
    pub fn start_from_env() -> Result<App> {
        App::start(config::analyst_home(), StartOptions::from_env())
    }

    /// The workbench of the desktop shell and the development bridge.
    pub fn workbench(&self) -> Result<Arc<Workbench>> {
        let mut opts = WorkbenchOptions::new(self.home.clone());
        opts.sandbox = self.sandbox.clone();
        Ok(Arc::new(Workbench::new(opts).map_err(|e| anyhow!("{e}"))?))
    }

    /// `<home>/ssh`, where SSH control sockets live.
    pub fn control_dir(&self) -> Option<PathBuf> {
        let d = self.home.join("ssh");
        std::fs::create_dir_all(&d).ok()?;
        Some(d)
    }

    /// The target named `name` (default: the configured default target).
    pub fn target(&self, name: Option<&str>) -> Result<Target> {
        let name = name.unwrap_or(&self.global.defaults.target);
        Target::from_config(&config::find_target(&self.global, name)?)
    }

    /// Whether a remote kernel can be wrapped in `bwrap --unshare-net` on its host.
    pub async fn remote_no_network(&self, target: &Target) -> bool {
        match target {
            Target::Ssh { host, .. } => {
                crate::egress::remote_bwrap_works(host, self.control_dir().as_deref())
                    .await
                    .unwrap_or(false)
            }
            Target::Local => false,
        }
    }

    /// The kernel transport for `repo` on `target`, with no workbench (the CLI). The local
    /// kernel runs in the tooling interpreter, without the provider keys in its environment.
    pub fn transport(
        &self,
        repo: &Path,
        project: &ProjectConfig,
        target: &Target,
        remote_no_network: bool,
    ) -> Box<dyn KernelTransport> {
        kernel_transport(KernelSpec {
            home: &self.home,
            repo,
            project,
            target,
            sandbox: self.sandbox.as_deref(),
            session_python: session_python(repo, None),
            local: LocalKernel::Tooling(crate::kernel::resolve_tooling_python_in(
                &self.global,
                &self.home,
            )),
            env_remove: crate::service::terminal::key_env_names(&self.global),
            control_dir: self.control_dir(),
            remote_no_network,
        })
    }

    /// A connected kernel for `repo` on `target` (the local tooling environment set up first
    /// when nothing else provides one). Warns on stderr when the kernel is not isolated from
    /// the network.
    pub async fn kernel(
        &self,
        repo: &Path,
        project: &ProjectConfig,
        target: &Target,
    ) -> Result<KernelClient> {
        if !target.is_remote() {
            self.ensure_local_tooling(|line| eprintln!("{line}")).await;
        }
        let t = self.transport(repo, project, target, self.remote_no_network(target).await);
        if t.network_isolation() == "none" {
            eprintln!(
                "warning: the kernel on `{}` is not isolated from the network (no working bwrap or sandbox-exec there)",
                target.name()
            );
        }
        tracing::info!("kernel: {}", t.describe());
        KernelClient::connect(t, crate::CORE_VERSION)
            .await
            .map_err(|e| anyhow!("starting the kernel on target `{}`: {e}\n  (set ANALYST_TOOLING_PYTHON or `tooling_python` in config.toml to the interpreter that has analyst_kernel installed)", target.name()))
    }

    /// Make sure a local tooling environment exists when nothing else provides one: the first
    /// start on a machine creates `$ANALYST_HOME/tooling/.venv` from the kernel source of a
    /// development checkout. `say` receives the progress lines.
    pub async fn ensure_local_tooling(&self, say: impl Fn(&str)) {
        if std::env::var_os("ANALYST_TOOLING_PYTHON").is_some_and(|v| !v.is_empty())
            || self
                .global
                .tooling_python
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        {
            return;
        }
        let resolved = crate::kernel::resolve_tooling_python_in(&self.global, &self.home);
        let bootstrapped = target::bootstrap::local_tooling_python(&self.home);
        let uses_bootstrapped = Path::new(&resolved) == bootstrapped;
        let dev_venv = resolved.contains("python/kernel/.venv");
        if dev_venv || (uses_bootstrapped && target::bootstrap::local_is_current(&self.home)) {
            return;
        }
        let Some(src) = target::bootstrap::find_dev_kernel_source() else {
            return;
        };
        say(&format!(
            "setting up the tooling environment in {} ...",
            self.home.join("tooling").display()
        ));
        if let Err(e) = target::bootstrap::ensure_local(&self.home, &src).await {
            say(&format!("warning: tooling environment setup failed: {e:#}"));
        }
    }

    /// The model client for `spec` (default: the configured default model) with its
    /// capabilities: cached, or probed and cached when missing or when `reprobe`. Keys come
    /// from the environment, `<home>/.env`, or `repo`'s `.env`.
    pub async fn model(
        &self,
        repo: &Path,
        spec: Option<&str>,
        reprobe: bool,
    ) -> Result<(Arc<dyn ModelClient>, ModelCapabilities, String)> {
        let spec = match spec {
            Some(s) => config::ModelSpec::parse(s)?,
            None => config::default_model(&self.global)?,
        };
        let resolved = config::resolve_model(&self.global, &spec)?;
        let client = crate::model_client::client_for_repo(&self.home, Some(repo), &resolved)?;
        let key = spec.to_string();
        let info_window = if reprobe || capabilities::cached(&self.home, &key).is_none() {
            crate::model_client::model_info_window(&self.home, Some(repo), &resolved).await
        } else {
            None
        };
        let caps = crate::model_client::ensure_capabilities(
            &self.home,
            client.as_ref(),
            &resolved,
            info_window,
            reprobe,
        )
        .await?;
        Ok((client, caps, key))
    }
}

/// The session interpreter of a repository: `override_` when given, else the repository's
/// `.venv`, else `python3`.
pub fn session_python(repo: &Path, override_: Option<&str>) -> String {
    if let Some(p) = override_ {
        return p.to_string();
    }
    let venv = repo.join(".venv/bin/python");
    if venv.is_file() {
        venv.to_string_lossy().to_string()
    } else {
        "python3".to_string()
    }
}

/// How the local kernel is started.
pub enum LocalKernel {
    /// `<tooling python> -m analyst_kernel`.
    Tooling(String),
    /// A launcher command line (tests: the fake kernel).
    Launcher(Vec<String>),
}

/// Everything [`kernel_transport`] needs.
pub struct KernelSpec<'a> {
    pub home: &'a Path,
    pub repo: &'a Path,
    pub project: &'a ProjectConfig,
    pub target: &'a Target,
    /// The sandbox kind asked for; `None`: `auto` locally, the target's setting remotely.
    pub sandbox: Option<&'a str>,
    pub session_python: String,
    pub local: LocalKernel,
    /// Environment variables the local kernel and its scripts do not get (provider keys).
    pub env_remove: Vec<String>,
    pub control_dir: Option<PathBuf>,
    /// Wrap a remote kernel in `bwrap --unshare-net` (the host's probe said it works). The
    /// local kernel is always spawned through the no-network wrapper.
    pub remote_no_network: bool,
}

/// The kernel transport for a repository on a target: the one construction used by the session
/// runtime and the CLI.
pub fn kernel_transport(k: KernelSpec<'_>) -> Box<dyn KernelTransport> {
    let eff = config::effective_project(k.project, k.target.name());
    match k.target {
        Target::Local => {
            let runs = k.home.join("runs");
            let _ = std::fs::create_dir_all(&runs);
            let args = KernelArgs {
                python: k.session_python,
                root: k.repo.to_string_lossy().to_string(),
                runs_dir: runs.to_string_lossy().to_string(),
                out_dir: Some(eff.out_dir),
                sandbox: Some(k.sandbox.unwrap_or("auto").to_string()),
                data_dirs: vec![],
            };
            let t = match k.local {
                LocalKernel::Launcher(l) => LocalStdio::with_launcher(l, args),
                LocalKernel::Tooling(python) => LocalStdio::new(python, args),
            };
            // The kernel and the scripts it runs never see provider API keys (U11).
            Box::new(t.without_env(k.env_remove))
        }
        Target::Ssh { host, sandbox, .. } => {
            let mirror = k.target.mirror_dir(k.repo).unwrap_or_default();
            Box::new(SshStdio {
                host: host.clone(),
                control_dir: k.control_dir,
                remote_launcher: vec![
                    "~/.analyst/tooling/.venv/bin/python".into(),
                    "-m".into(),
                    "analyst_kernel".into(),
                ],
                args: KernelArgs {
                    python: format!("{mirror}/.venv/bin/python"),
                    root: mirror,
                    runs_dir: "~/.analyst/runs".into(),
                    out_dir: Some(eff.out_dir),
                    sandbox: Some(
                        k.sandbox
                            .map(str::to_string)
                            .unwrap_or_else(|| sandbox.clone()),
                    ),
                    // User-approved external data directories from the global config.
                    data_dirs: k.target.data_dirs().to_vec(),
                },
                no_network: k.remote_no_network,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_creates_the_home_and_loads_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let app = App::start(
            home.clone(),
            StartOptions {
                sandbox: Some("limits".into()),
            },
        )
        .unwrap();
        assert!(home.is_dir());
        assert_eq!(app.sandbox.as_deref(), Some("limits"));
        assert!(app.migration_error.is_none());
        let wb = app.workbench().unwrap();
        assert_eq!(wb.home(), home.as_path());
    }

    #[test]
    fn the_local_kernel_runs_from_the_tooling_interpreter_with_the_sandbox_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::start(
            dir.path().join("home"),
            StartOptions {
                sandbox: Some("limits".into()),
            },
        )
        .unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let t = app.transport(&repo, &ProjectConfig::default(), &Target::Local, false);
        let line = t.describe();
        assert!(line.contains("analyst_kernel"), "{line}");
        assert!(line.contains("limits"), "{line}");
        assert!(dir.path().join("home/runs").is_dir());
    }
}
