//! `analyst`: the command line of Personal Analyst: analysis (scripts, runs, checks), decks
//! (`analyst deck ...`), the one agent loop (`analyst chat`), and evaluations of both.

mod deck;

use analyst_core::agent::{Agent, AgentConfig, ApprovalRequest, Approver, AutoApprover};
use analyst_core::config::{self, ModelSpec};
use analyst_core::kernel::{
    resolve_tooling_python_in, KernelArgs, KernelClient, KernelTransport, LocalStdio, SshStdio,
};
use analyst_core::model_client;
use analyst_core::session::Session;
use analyst_core::target::{self, Target};
use analyst_core::tools::gate::Policy;
use analyst_core::tools::ToolContext;
use analyst_protocol::config::{EditPolicy, GlobalConfig, ProjectConfig, RunPolicy, TargetKind};
use analyst_protocol::evals::Mode;
use analyst_protocol::kernel::*;
use analyst_protocol::transcript::Event;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use clap::{Args, Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use wb_providers::model::ModelCapabilities;
use wb_providers::{capabilities, ModelClient};

#[derive(Parser)]
#[command(
    name = "analyst",
    version,
    about = "Personal Analyst: agent-driven, reusable Python analysis tools"
)]
struct Cli {
    /// Analyst home (default: $ANALYST_HOME or ~/.analyst).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Repository root (default: current directory).
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    /// Verbose logging (-v info, -vv debug).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Sandbox kind for runs on the chosen target: auto, bwrap, sandbox-exec, limits, none.
    /// Overrides the target's setting; use `limits` to diagnose a denial from the OS sandbox.
    #[arg(
        long,
        global = true,
        value_name = "KIND",
        value_parser = ["auto", "bwrap", "sandbox-exec", "limits", "none"]
    )]
    sandbox: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a template repo (.analyst/project.toml, scripts/, analysis/, data/, out/, pyproject.toml, uv venv, git init).
    Init { path: Option<PathBuf> },
    /// Manage the project environment with uv.
    Env {
        #[command(subcommand)]
        cmd: EnvCmd,
        #[arg(long)]
        target: Option<String>,
    },
    /// Execution targets.
    Target {
        #[command(subcommand)]
        cmd: TargetCmd,
    },
    /// Run a script on the target and stream its output. Runs have no network access.
    Run {
        script: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        timeout_s: Option<f64>,
        /// Script arguments (after `--`).
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Contract check + lint; exits non-zero on errors.
    Check {
        paths: Vec<String>,
        #[arg(long)]
        target: Option<String>,
    },
    /// Extract equations from a file; `--check` verifies declared equations numerically.
    Eq {
        path: String,
        #[arg(long)]
        check: bool,
        #[arg(long)]
        function: Option<String>,
        #[arg(long)]
        target: Option<String>,
    },
    /// Print the script and library indexes.
    Index {
        #[arg(long)]
        target: Option<String>,
    },
    /// Chat with the agent (interactive REPL when no message is given).
    Chat(ChatArgs),
    /// Evaluation suite.
    Eval {
        #[command(subcommand)]
        cmd: EvalCmd,
    },
    /// Raw JSON-RPC passthrough to the kernel (stdin lines -> kernel, kernel stdout -> stdout).
    Kernel {
        #[arg(long)]
        target: Option<String>,
    },
    /// Stored provider API keys ($ANALYST_HOME/.env, mode 0600).
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
    /// Decks of the repository (`decks/<slug>/`): create, references, render, check, export,
    /// code mode, and the undo timeline.
    Deck(deck::DeckArgs),
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Store the key for a provider; the key is read from stdin (never from the command line).
    Set { provider: String },
    /// Remove the stored key for a provider.
    Remove { provider: String },
    /// Show which providers have a key and where it comes from.
    Status,
}

#[derive(Subcommand)]
enum EnvCmd {
    List,
    Add { packages: Vec<String> },
    Remove { packages: Vec<String> },
    Sync,
}

#[derive(Subcommand)]
enum TargetCmd {
    List,
    Bootstrap { name: String },
    Sync { name: String },
    Info { name: Option<String> },
}

#[derive(Args)]
struct ChatArgs {
    /// `provider/model` (default from config).
    #[arg(short, long)]
    model: Option<String>,
    /// plan, build, run (analysis) or outline, slides, polish (decks); build covers both.
    #[arg(long, default_value = "build")]
    mode: String,
    /// edits=auto, runs=auto and approve every remaining question.
    #[arg(long)]
    auto: bool,
    #[arg(long)]
    target: Option<String>,
    #[arg(long, default_value_t = 40)]
    max_steps: u32,
    /// Re-run the capability probe.
    #[arg(long)]
    reprobe: bool,
    /// Rendering engine for deck kernels: auto, powerpoint, libreoffice, none.
    #[arg(
        long,
        default_value = "auto",
        value_parser = ["auto", "powerpoint", "libreoffice", "none"]
    )]
    engine: String,
    message: Option<String>,
}

#[derive(Subcommand)]
enum EvalCmd {
    /// Run analysis tasks and deck tasks (a deck task's fixture names a template in a
    /// `TEMPLATE` file).
    Run {
        /// `provider/model` (default from config).
        #[arg(long, short)]
        model: Option<String>,
        #[arg(long, default_value = "evals/tasks")]
        tasks: PathBuf,
        #[arg(long)]
        task: Option<String>,
        /// edits=auto, runs=auto for analysis tasks (deck tasks always apply edits inside
        /// the declared scope).
        #[arg(long)]
        auto: bool,
        #[arg(long, default_value = "evals/results")]
        results: PathBuf,
        /// Templates of deck tasks (default: `<tasks>/../templates`).
        #[arg(long)]
        templates: Option<PathBuf>,
        /// Rendering engine for deck tasks: auto, powerpoint, libreoffice, none.
        #[arg(
            long,
            default_value = "auto",
            value_parser = ["auto", "powerpoint", "libreoffice", "none"]
        )]
        engine: String,
    },
}

struct EvalOpts {
    model: Option<String>,
    tasks: PathBuf,
    task: Option<String>,
    auto: bool,
    results: PathBuf,
    templates: Option<PathBuf>,
    engine: String,
}

struct App {
    home: PathBuf,
    repo: PathBuf,
    global: GlobalConfig,
    project: ProjectConfig,
    sandbox: Option<String>,
}

impl App {
    fn new(cli: &Cli) -> Result<App> {
        let home = cli.home.clone().unwrap_or_else(config::analyst_home);
        std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
        let repo = cli.repo.clone().unwrap_or(std::env::current_dir()?);
        let repo = std::fs::canonicalize(&repo).unwrap_or(repo);
        migrate_legacy_home(&home);
        // The network gate records every decision in `<home>/egress.jsonl`; loading the
        // config registers the destinations it names (providers, package index, SSH targets).
        wb_net::init(&home);
        let global = config::load_global(&home)?;
        let project = config::load_project(&repo)?;
        Ok(App {
            home,
            repo,
            global,
            project,
            sandbox: cli.sandbox.clone(),
        })
    }

    fn target(&self, name: Option<&str>) -> Result<Target> {
        let name = name.unwrap_or(&self.global.defaults.target);
        Target::from_config(&config::find_target(&self.global, name)?)
    }

    fn control_dir(&self) -> Option<PathBuf> {
        let d = self.home.join("ssh");
        std::fs::create_dir_all(&d).ok()?;
        Some(d)
    }

    fn session_python(&self) -> String {
        let venv = self.repo.join(".venv/bin/python");
        if venv.is_file() {
            venv.to_string_lossy().to_string()
        } else {
            "python3".to_string()
        }
    }

    /// Whether a remote kernel can be wrapped in `bwrap --unshare-net` on its host.
    async fn remote_no_network(&self, target: &Target) -> bool {
        match target {
            Target::Ssh { host, .. } => {
                analyst_core::egress::remote_bwrap_works(host, self.control_dir().as_deref())
                    .await
                    .unwrap_or(false)
            }
            Target::Local => false,
        }
    }

    /// The kernel transport. The local kernel is spawned without network access; a remote
    /// one is when `remote_no_network` (see [`Self::remote_no_network`]).
    fn transport(&self, target: &Target, remote_no_network: bool) -> Box<dyn KernelTransport> {
        let eff = config::effective_project(&self.project, target.name());
        match target {
            Target::Local => {
                let runs = self.home.join("runs");
                let _ = std::fs::create_dir_all(&runs);
                Box::new(
                    LocalStdio::new(
                        resolve_tooling_python_in(&self.global, &self.home),
                        KernelArgs {
                            python: self.session_python(),
                            root: self.repo.to_string_lossy().to_string(),
                            runs_dir: runs.to_string_lossy().to_string(),
                            out_dir: Some(eff.out_dir),
                            sandbox: Some(self.sandbox.clone().unwrap_or_else(|| "auto".into())),
                            data_dirs: vec![],
                        },
                    )
                    // The kernel and the scripts it runs never see provider API keys.
                    .without_env(analyst_core::service::terminal::key_env_names(&self.global)),
                )
            }
            Target::Ssh {
                host,
                sandbox,
                data_dirs,
                ..
            } => {
                let mirror = target.mirror_dir(&self.repo).unwrap_or_default();
                Box::new(SshStdio {
                    host: host.clone(),
                    control_dir: self.control_dir(),
                    remote_launcher: vec![
                        "~/.analyst/tooling/.venv/bin/python".into(),
                        "-m".into(),
                        "analyst_kernel".into(),
                    ],
                    args: KernelArgs {
                        python: format!("{mirror}/.venv/bin/python"),
                        root: mirror.clone(),
                        runs_dir: "~/.analyst/runs".into(),
                        out_dir: Some(eff.out_dir),
                        sandbox: Some(self.sandbox.clone().unwrap_or_else(|| sandbox.clone())),
                        // User-approved external data directories from the global config.
                        data_dirs: data_dirs.clone(),
                    },
                    no_network: remote_no_network,
                })
            }
        }
    }

    async fn connect(&self, target: &Target) -> Result<KernelClient> {
        if !target.is_remote() {
            self.ensure_local_tooling().await;
        }
        let t = self.transport(target, self.remote_no_network(target).await);
        if t.network_isolation() == "none" {
            eprintln!(
                "warning: the kernel on `{}` is not isolated from the network (no working bwrap or sandbox-exec there)",
                target.name()
            );
        }
        tracing::info!("kernel: {}", t.describe());
        KernelClient::connect(t, analyst_core::CORE_VERSION)
            .await
            .map_err(|e| anyhow!("starting the kernel on target `{}`: {e}\n  (set ANALYST_TOOLING_PYTHON or `tooling_python` in config.toml to the interpreter that has analyst_kernel installed)", target.name()))
    }

    /// Make sure a local tooling environment exists when nothing else provides one: the
    /// first start on a machine creates `$ANALYST_HOME/tooling/.venv` from the kernel source.
    async fn ensure_local_tooling(&self) {
        if std::env::var_os("ANALYST_TOOLING_PYTHON").is_some_and(|v| !v.is_empty())
            || self
                .global
                .tooling_python
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        {
            return;
        }
        let resolved = resolve_tooling_python_in(&self.global, &self.home);
        let bootstrapped = target::bootstrap::local_tooling_python(&self.home);
        let uses_bootstrapped = std::path::Path::new(&resolved) == bootstrapped;
        let dev_venv = resolved.contains("python/kernel/.venv");
        if dev_venv || (uses_bootstrapped && target::bootstrap::local_is_current(&self.home)) {
            return;
        }
        let Some(src) = target::bootstrap::find_dev_kernel_source() else {
            return;
        };
        eprintln!(
            "setting up the tooling environment in {} ...",
            self.home.join("tooling").display()
        );
        if let Err(e) = target::bootstrap::ensure_local(&self.home, &src).await {
            eprintln!("warning: tooling environment setup failed: {e:#}");
        }
    }

    fn session(&self, target: &Target, model: &str) -> Result<Session> {
        match Session::find_latest(&self.home, &self.repo, target.name())? {
            Some(s) => Ok(s),
            None => Session::create(&self.home, &self.repo, target.name(), model),
        }
    }

    async fn model(
        &self,
        spec: Option<&str>,
        reprobe: bool,
    ) -> Result<(Arc<dyn ModelClient>, ModelCapabilities, String)> {
        let spec = match spec {
            Some(s) => ModelSpec::parse(s)?,
            None => config::default_model(&self.global)?,
        };
        let resolved = config::resolve_model(&self.global, &spec)?;
        let client = model_client::client_for_repo(&self.home, Some(&self.repo), &resolved)?;
        let key = spec.to_string();
        let info_window = if reprobe || capabilities::cached(&self.home, &key).is_none() {
            model_client::model_info_window(&self.home, Some(&self.repo), &resolved).await
        } else {
            None
        };
        let caps = model_client::ensure_capabilities(
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

/// First start: bring the deck workbench's old home (`~/.present`) over once.
fn migrate_legacy_home(home: &std::path::Path) {
    match analyst_core::migrate::migrate_legacy_home(home) {
        Ok(Some(r)) => eprintln!(
            "migrated {} into {}: {} provider(s), {} file(s), {} key(s) (the old folder is left as it was)",
            r.from.display(),
            home.display(),
            r.providers.len(),
            r.files.len(),
            r.keys.len()
        ),
        Ok(None) => {}
        Err(e) => eprintln!("warning: migrating ~/.present failed: {e:#}"),
    }
}

struct InteractiveApprover;

#[async_trait]
impl Approver for InteractiveApprover {
    async fn approve(&self, req: &ApprovalRequest) -> bool {
        println!();
        println!("── approval needed: {} ({})", req.tool, req.reason);
        if let Some(p) = &req.proposal {
            println!("   {}", p.summary);
            for (i, line) in p.diff.lines().enumerate() {
                if i >= 120 {
                    println!("   ... (diff truncated)");
                    break;
                }
                println!("   {line}");
            }
        } else {
            println!(
                "   {}",
                serde_json::to_string(&req.args).unwrap_or_default()
            );
        }
        print!("   accept? [y/N] ");
        let _ = std::io::stdout().flush();
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            s
        })
        .await
        .unwrap_or_default();
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

fn print_diagnostics(diags: &[Diagnostic]) -> usize {
    let mut errors = 0;
    for d in diags {
        let sev = match d.severity {
            Severity::Error => {
                errors += 1;
                "error"
            }
            Severity::Warn => "warn ",
        };
        println!(
            "{sev} {}:{}:{} {} {}",
            d.path, d.line, d.col, d.rule, d.message
        );
        if let Some(h) = &d.hint {
            println!("      hint: {h}");
        }
    }
    errors
}

fn cmd_init(home: Option<PathBuf>, path: Option<PathBuf>) -> Result<()> {
    let root = path.unwrap_or(std::env::current_dir()?);
    // `uv venv` and `uv sync` install from the configured package index, through the gate.
    let home = home.unwrap_or_else(config::analyst_home);
    migrate_legacy_home(&home);
    wb_net::init(&home);
    config::load_global(&home)?;
    let report = analyst_core::init::init_repo(&root, &analyst_core::init::InitOptions::default())?;
    for rel in &report.kept {
        println!("keep   {rel}");
    }
    for rel in &report.created {
        println!("create {rel}");
    }
    for line in &report.log {
        println!("{line}");
    }
    println!("ready: {}", report.root.display());
    Ok(())
}

async fn cmd_env(app: &App, cmd: EnvCmd, target: Option<String>) -> Result<()> {
    let target = app.target(target.as_deref())?;
    let (tool, pkgs) = match cmd {
        EnvCmd::List => {
            match &target {
                Target::Local => {
                    let line = vec![
                        "uv".to_string(),
                        "pip".into(),
                        "list".into(),
                        "--python".into(),
                        app.session_python(),
                    ];
                    let out = target::run_command(&line, Some(&app.repo)).await?;
                    print!("{}", String::from_utf8_lossy(&out.stdout));
                    eprint!("{}", String::from_utf8_lossy(&out.stderr));
                }
                Target::Ssh { host, .. } => {
                    let mirror = target.mirror_dir(&app.repo).unwrap_or_default();
                    let remote = format!(
                        "cd {} && UV_OFFLINE=1 ~/.analyst/bin/uv pip list",
                        analyst_core::util::sh_quote_path(&mirror)
                    );
                    print!(
                        "{}",
                        target::run_ssh(host, app.control_dir().as_deref(), &remote).await?
                    );
                }
            }
            return Ok(());
        }
        EnvCmd::Add { packages } => ("pkg.add", packages),
        EnvCmd::Remove { packages } => ("pkg.remove", packages),
        EnvCmd::Sync => ("pkg.sync", vec![]),
    };
    // The same code path as the agent's package tools: names are validated, and on a remote
    // target the change is made locally, pushed, and applied there with `uv sync`.
    let session = app.session(&target, app.global.defaults.model.as_deref().unwrap_or("-"))?;
    let mut ctx = ToolContext::new(&app.repo, &app.home, session, "cli", app.project.clone());
    ctx.set_target(target.clone(), app.control_dir());
    let v = ctx
        .dispatch("cli-env", tool, &serde_json::json!({"packages": pkgs}))
        .await
        .map_err(|e| anyhow!("{}", e.message))?;
    for key in ["stdout", "stderr", "output"] {
        if let Some(t) = v[key].as_str() {
            if !t.is_empty() {
                println!("{}", t.trim_end());
            }
        }
    }
    Ok(())
}

fn cmd_key(app: &App, cmd: KeyCmd) -> Result<()> {
    let known = |provider: &str| -> Result<&analyst_protocol::config::ProviderConfig> {
        if let Some(p) = app.global.providers.iter().find(|p| p.name == provider) {
            return Ok(p);
        }
        Err(anyhow!(
            "provider `{provider}` is not configured in config.toml (known: {})",
            app.global
                .providers
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };
    match cmd {
        KeyCmd::Set { provider } => {
            let p = known(&provider)?;
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                eprint!("API key for `{provider}` (input is read from stdin, then press Enter): ");
            }
            let mut key = String::new();
            std::io::stdin()
                .read_line(&mut key)
                .context("reading the key from stdin")?;
            let key = key.trim();
            if key.is_empty() {
                return Err(anyhow!(
                    "no key given on stdin (use `analyst key remove {provider}` to delete one)"
                ));
            }
            config::set_api_key(&app.home, p, key)?;
            println!(
                "stored the key for `{provider}` as {} in {}",
                config::key_env_name(p),
                config::env_file_path(&app.home).display()
            );
        }
        KeyCmd::Remove { provider } => {
            let p = known(&provider)?;
            config::set_api_key(&app.home, p, "")?;
            println!(
                "removed {} from {}",
                config::key_env_name(p),
                config::env_file_path(&app.home).display()
            );
        }
        KeyCmd::Status => {
            for p in &app.global.providers {
                let source =
                    match config::api_key_lookup(&app.home, Some(&app.repo), p).map(|(_, s)| s) {
                        Some(config::KeySource::Env) => "environment",
                        Some(config::KeySource::Stored) => ".env file",
                        None => "none",
                    };
                println!("{}  key={source}  env={}", p.name, config::key_env_name(p));
            }
        }
    }
    Ok(())
}

async fn cmd_target(app: &App, cmd: TargetCmd) -> Result<()> {
    match cmd {
        TargetCmd::List => {
            println!(
                "local  (default: {})",
                if app.global.defaults.target == "local" {
                    "yes"
                } else {
                    "no"
                }
            );
            for t in &app.global.targets {
                let kind = match t.kind {
                    TargetKind::Local => "local",
                    TargetKind::Ssh => "ssh",
                };
                println!(
                    "{}  kind={kind} host={} remote_root={} python={} sandbox={} data_dirs={:?}{}",
                    t.name,
                    t.host.as_deref().unwrap_or("-"),
                    t.remote_root.as_deref().unwrap_or("~/analyst"),
                    t.python.as_deref().unwrap_or("3.12"),
                    t.sandbox.as_deref().unwrap_or("auto"),
                    t.data_dirs,
                    if app.global.defaults.target == t.name {
                        " (default)"
                    } else {
                        ""
                    }
                );
            }
        }
        TargetCmd::Bootstrap { name } => {
            let target = app.target(Some(&name))?;
            let wheel = target::bootstrap::find_dev_wheel();
            match target::bootstrap::ensure(&target, app.control_dir().as_deref(), wheel.as_deref())
                .await?
            {
                target::bootstrap::Outcome::UpToDate(v) => {
                    println!("{name}: tooling {v} already installed")
                }
                target::bootstrap::Outcome::Installed(v) => {
                    println!("{name}: installed tooling {v}")
                }
            }
            if let Target::Ssh { host, .. } = &target {
                let mirror = target.mirror_dir(&app.repo).unwrap_or_default();
                let index_flag = analyst_core::egress::remote_index_flag("uv sync (remote)")?;
                let command = target::bootstrap::session_env_command(&mirror, &index_flag);
                println!("session env: {command}");
                target::run_ssh(host, app.control_dir().as_deref(), &command).await?;
                println!("{name}: session environment ready in {mirror}");
            }
        }
        TargetCmd::Sync { name } => {
            let target = app.target(Some(&name))?;
            let mut session =
                app.session(&target, app.global.defaults.model.as_deref().unwrap_or("-"))?;
            let report = target::sync::push(
                &app.repo,
                &target,
                app.control_dir().as_deref(),
                session.record.last_tree_hash.as_deref(),
                false,
            )
            .await?;
            session.set_last_tree_hash(Some(report.tree_hash.clone()))?;
            println!(
                "{name}: {:?} {} files, tree {}",
                report.method,
                report.files,
                &report.tree_hash[..12]
            );
            if let Some(w) = report.warning {
                println!("warning: {w}");
            }
        }
        TargetCmd::Info { name } => {
            let target = app.target(name.as_deref())?;
            println!(
                "target {} ({})",
                target.name(),
                if target.is_remote() { "ssh" } else { "local" }
            );
            if let Some(m) = target.mirror_dir(&app.repo) {
                println!("mirror {m}");
            }
            let k = app.connect(&target).await?;
            if let Some(h) = k.hello().await {
                println!(
                    "kernel {} protocol {} host {} os {} sandbox {}",
                    h.kernel_version, h.protocol_version, h.host, h.os, h.sandbox
                );
                println!("python {}  root {}  runs {}", h.python, h.root, h.runs_dir);
            }
            let info = k.env_info().await?;
            println!(
                "version {}  venv {}  uv {}",
                info.version,
                info.venv.as_deref().unwrap_or("-"),
                info.uv.as_deref().unwrap_or("-")
            );
            k.shutdown().await?;
        }
    }
    Ok(())
}

async fn cmd_run(
    app: &App,
    script: String,
    args: Vec<String>,
    target: Option<String>,
    timeout_s: Option<f64>,
) -> Result<i32> {
    let target = app.target(target.as_deref())?;
    let session = app.session(&target, app.global.defaults.model.as_deref().unwrap_or("-"))?;
    let kernel = app.connect(&target).await?;
    let mut ctx = ToolContext::new(&app.repo, &app.home, session, "cli", app.project.clone());
    ctx.set_kernel(Some(kernel.clone()));
    ctx.set_target(target.clone(), app.control_dir());
    ctx.stream_run_output(Arc::new(|stream, chunk| {
        if stream == "stdout" {
            print!("{chunk}");
            let _ = std::io::stdout().flush();
        } else {
            eprint!("{chunk}");
        }
    }));
    let mut a = serde_json::json!({"script": script, "args": args});
    if let Some(t) = timeout_s {
        a["timeout_s"] = serde_json::json!(t);
    }
    let result = tokio::select! {
        r = ctx.dispatch("cli-run", "run.script", &a) => r,
        _ = tokio::signal::ctrl_c() => {
            // Ctrl-C: stop the script on the target before leaving, not just this process.
            eprintln!("\ninterrupted: cancelling the run ...");
            cancel_active_runs(&kernel).await;
            let _ = kernel.shutdown().await;
            return Ok(130);
        }
    };
    let _ = kernel.shutdown().await;
    let v = result.map_err(|e| anyhow!("{}", e.message))?;
    println!();
    println!(
        "run {} {} exit={} ({:.1}s) out={}",
        v["run_id"].as_str().unwrap_or(""),
        v["status"].as_str().unwrap_or(""),
        v["exit_code"],
        v["duration_s"].as_f64().unwrap_or(0.0),
        v["out_dir"].as_str().unwrap_or("")
    );
    for art in v["artifacts"].as_array().unwrap_or(&vec![]) {
        println!(
            "  {:<7} {} ({} bytes)",
            art["kind"].as_str().unwrap_or(""),
            art["path"].as_str().unwrap_or(""),
            art["size"]
        );
    }
    let code = v["exit_code"].as_i64().unwrap_or(1) as i32;
    Ok(if v["status"] == "done" { code } else { 1 })
}

/// `run.cancel` for every run this client started that has not finished.
async fn cancel_active_runs(kernel: &KernelClient) {
    for (run_id, _) in kernel.active_runs().await {
        match tokio::time::timeout(Duration::from_secs(10), kernel.run_cancel(&run_id)).await {
            Ok(Ok(_)) => eprintln!("cancelled run {run_id}"),
            Ok(Err(e)) => eprintln!("could not cancel run {run_id}: {e}"),
            Err(_) => eprintln!("could not cancel run {run_id}: the kernel did not answer"),
        }
    }
}

async fn cmd_check(app: &App, paths: Vec<String>, target: Option<String>) -> Result<i32> {
    let target = app.target(target.as_deref())?;
    let k = app.connect(&target).await?;
    let paths = if paths.is_empty() { None } else { Some(paths) };
    let mut diags = k.contract_check(paths.clone()).await?.diagnostics;
    match k.lint_run(paths).await {
        Ok(r) => diags.extend(r.diagnostics),
        Err(e) => eprintln!("lint unavailable: {e}"),
    }
    let _ = k.shutdown().await;
    diags.sort_by_key(|a| (a.path.clone(), a.line));
    let errors = print_diagnostics(&diags);
    println!("{} diagnostic(s), {errors} error(s)", diags.len());
    Ok(if errors > 0 { 1 } else { 0 })
}

async fn cmd_eq(
    app: &App,
    path: String,
    check: bool,
    function: Option<String>,
    target: Option<String>,
) -> Result<i32> {
    let target = app.target(target.as_deref())?;
    let k = app.connect(&target).await?;
    let r = k
        .eq_extract(EqExtractParams {
            path: path.clone(),
            function: function.clone(),
        })
        .await?;
    let mut mismatches = 0;
    for e in &r.equations {
        println!(
            "{}:{} {} [{:?}] {:?}",
            path, e.line, e.qualname, e.tier, e.status
        );
        if let Some(l) = &e.latex {
            println!("    latex: {l}");
        }
        if let Some(s) = &e.source_expr {
            println!("    expr:  {s}");
        }
        if let Some(reason) = &e.reason {
            println!("    note:  {reason}");
        }
        if check && e.latex.is_some() {
            let c = k
                .eq_check(EqCheckParams {
                    path: path.clone(),
                    function: e.function.clone(),
                    trials: None,
                    tol: None,
                    ranges: None,
                })
                .await?;
            println!(
                "    check: {:?} trials={} max_rel_err={:?} {}",
                c.status,
                c.trials,
                c.max_rel_err,
                c.reason.unwrap_or_default()
            );
            if let Some(ce) = c.counterexample {
                println!(
                    "    counterexample: inputs={:?} expected={} actual={}",
                    ce.inputs, ce.expected, ce.actual
                );
            }
            if c.status == EqCheckStatus::Mismatch {
                mismatches += 1;
            }
        }
    }
    let _ = k.shutdown().await;
    Ok(if mismatches > 0 { 1 } else { 0 })
}

async fn cmd_index(app: &App, target: Option<String>) -> Result<()> {
    let target = app.target(target.as_deref())?;
    let k = app.connect(&target).await?;
    let s = k
        .scripts_index(Some(app.project.project.scripts.clone()))
        .await?;
    println!("scripts ({})", s.scripts.len());
    for sc in &s.scripts {
        println!(
            "  {}  {}",
            sc.path,
            sc.description.as_deref().unwrap_or("(no description)")
        );
        for o in &sc.options {
            println!(
                "      {:<22} {:<6} default={} {}",
                o.flags.join("/"),
                o.r#type.as_deref().unwrap_or("-"),
                o.default,
                o.help.as_deref().unwrap_or("")
            );
        }
    }
    let l = k
        .lib_index(Some(app.project.project.library.clone()))
        .await?;
    println!("library functions ({})", l.functions.len());
    for f in &l.functions {
        println!(
            "  {}.{}{}  {}{}  callers={}",
            f.module,
            f.qualname,
            f.signature,
            f.doc_summary.as_deref().unwrap_or("(no docstring)"),
            if f.has_math { " [Math]" } else { "" },
            f.callers.len()
        );
    }
    let _ = k.shutdown().await;
    Ok(())
}

fn event_printer() -> analyst_core::agent::EventSink {
    Arc::new(|e: &Event| match e {
        Event::ToolCall {
            name, arguments, ..
        } => {
            let a = serde_json::to_string(arguments).unwrap_or_default();
            println!("\n» {name} {}", analyst_core::util::truncate_bytes(&a, 160));
        }
        Event::ToolResult {
            ok,
            result,
            truncated,
            ..
        } => {
            let r = serde_json::to_string(result).unwrap_or_default();
            println!(
                "  {} {}{}",
                if *ok { "ok" } else { "ERR" },
                analyst_core::util::truncate_bytes(&r, 160),
                if truncated.unwrap_or(false) {
                    " (truncated)"
                } else {
                    ""
                }
            );
        }
        Event::Run {
            run_id,
            script,
            args,
            target,
            ..
        } => println!("  run {run_id}: {script} {} on {target}", args.join(" ")),
        Event::Decision {
            proposal_id,
            decision,
            by,
            ..
        } => println!("  decision {proposal_id}: {decision:?} by {by:?}"),
        _ => {}
    })
}

async fn cmd_chat(app: &App, args: ChatArgs) -> Result<()> {
    let mode: Mode = args.mode.parse().map_err(|e: String| anyhow!(e))?;
    let target = app.target(args.target.as_deref())?;
    let (model, caps, model_name) = app.model(args.model.as_deref(), args.reprobe).await?;
    println!(
        "model {model_name}  tools={:?} context={} target={} mode={mode}",
        caps.tools,
        caps.context_window,
        target.name()
    );
    let kernel = match app.connect(&target).await {
        Ok(k) => Some(k),
        Err(e) => {
            eprintln!("warning: {e}\n(continuing without a kernel: index, run, and check tools are unavailable)");
            None
        }
    };
    let session = app.session(&target, &model_name)?;
    let conv_id = analyst_core::util::stamped_id();
    println!("session {} conversation {}", session.id(), conv_id);
    let mut project = app.project.clone();
    if args.auto {
        project.policy.edits = EditPolicy::Auto;
        project.policy.runs = RunPolicy::Auto;
    }
    project.policy.max_steps = args.max_steps;
    let mut ctx = ToolContext::new(&app.repo, &app.home, session, &conv_id, project.clone());
    ctx.set_kernel(kernel.clone());
    ctx.set_target(target.clone(), app.control_dir());
    // Deck tools start a deck's kernel (no network) on first use.
    ctx.decks.set_options(app.deck_options(&args.engine));
    let decks_found = ctx.decks.list();
    if !decks_found.is_empty() {
        println!(
            "decks: {}",
            decks_found
                .iter()
                .map(|d| d.slug.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    ctx.stream_run_output(Arc::new(|stream, chunk| {
        if stream == "stdout" {
            print!("{chunk}");
        } else {
            eprint!("{chunk}");
        }
    }));
    let mut policy = Policy::for_project(&app.repo, &project, mode)?;
    // The policy must know the sandbox before the first tool call: without a confining
    // sandbox, runs ask even under `runs = "auto"`.
    if let Some(k) = &kernel {
        if let Some(h) = k.hello().await {
            policy.set_sandbox(&h.sandbox);
            if !policy.is_sandboxed() {
                eprintln!(
                    "note: sandbox is `{}` on this target, so runs are not confined; every run asks first{}",
                    h.sandbox,
                    if args.auto { " (auto-approved by --auto)" } else { "" }
                );
            }
        }
    }
    let approver: Arc<dyn Approver> = if args.auto {
        Arc::new(AutoApprover)
    } else {
        Arc::new(InteractiveApprover)
    };
    let config = AgentConfig::new(mode, &model_name, caps, args.max_steps);
    let mut agent = Agent::new(model, ctx, policy, approver, config);
    agent.on_text = Some(Arc::new(|t| {
        print!("{t}");
        let _ = std::io::stdout().flush();
    }));
    agent.on_event = Some(event_printer());

    let mut turns = 0u32;
    if let Some(m) = args.message {
        turns += 1;
        if run_one(&mut agent, &m, &format!("{conv_id}-{turns}")).await {
            if let Some(k) = kernel {
                let _ = k.shutdown().await;
            }
            std::process::exit(130);
        }
    } else {
        println!("interactive; /mode plan|build|run|outline|slides|polish, /quit");
        loop {
            print!("\n{}> ", agent.config.mode);
            let _ = std::io::stdout().flush();
            let line = tokio::task::spawn_blocking(|| {
                let mut s = String::new();
                match std::io::stdin().read_line(&mut s) {
                    Ok(0) => None,
                    Ok(_) => Some(s),
                    Err(_) => None,
                }
            })
            .await?;
            let Some(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line == "/quit" || line == "/exit" {
                break;
            }
            if let Some(m) = line.strip_prefix("/mode ") {
                match m.trim().parse::<Mode>() {
                    Ok(m) => {
                        agent.set_mode(m);
                        println!("mode = {m}");
                    }
                    Err(e) => println!("{e}"),
                }
                continue;
            }
            // Ctrl-C during a turn cancels the turn and its run, and returns to the prompt.
            turns += 1;
            run_one(&mut agent, line, &format!("{conv_id}-{turns}")).await;
        }
    }
    if let Some(k) = kernel {
        let _ = k.shutdown().await;
    }
    Ok(())
}

/// Run one turn. Returns true when it was interrupted by Ctrl-C: the turn is dropped, any run
/// it started is cancelled on the target, and the agent is left ready for the next message.
async fn run_one(agent: &mut Agent, text: &str, turn: &str) -> bool {
    let outcome = tokio::select! {
        r = agent.run_turn(text) => Some(r),
        _ = tokio::signal::ctrl_c() => None,
    };
    let Some(result) = outcome else {
        eprintln!("\ninterrupted: cancelling the turn ...");
        if let Some(k) = agent.ctx.kernel.clone() {
            cancel_active_runs(&k).await;
        }
        agent.recover_after_cancel();
        return true;
    };
    match result {
        Ok(s) => {
            println!();
            println!(
                "── turn {turn}: {} step(s), {} tool call(s) ({} error(s)), tokens {}/{}",
                s.steps, s.tool_calls, s.tool_errors, s.input_tokens, s.output_tokens
            );
            if !s.files_changed.is_empty() {
                println!("   files: {}", s.files_changed.join(", "));
            }
            if !s.runs.is_empty() {
                println!("   runs: {}", s.runs.join(", "));
            }
            if !s.figures.is_empty() {
                println!("   figures: {}", s.figures.join(", "));
            }
            for i in &s.open_issues {
                println!("   open: {i}");
            }
        }
        Err(e) => eprintln!("\nturn failed: {e:#}"),
    }
    false
}

async fn cmd_eval(app: &App, o: EvalOpts) -> Result<i32> {
    use analyst_core::evals::deck as deck_evals;
    // Analysis tasks and deck tasks can share a folder; a filter must match one of them.
    let analysis = match analyst_core::evals::load_tasks(&o.tasks, o.task.as_deref()) {
        Ok(t) => t,
        Err(_) if o.task.is_some() => vec![],
        Err(e) => return Err(e),
    };
    let decks = match deck_evals::load_tasks(&o.tasks, o.task.as_deref()) {
        Ok(t) => t,
        Err(_) if o.task.is_some() => vec![],
        Err(e) => return Err(e),
    };
    if analysis.is_empty() && decks.is_empty() {
        return Err(anyhow!(
            "no task{} under {}",
            o.task
                .as_deref()
                .map(|t| format!(" named `{t}`"))
                .unwrap_or_default(),
            o.tasks.display()
        ));
    }
    let (client, caps, model_name) = app.model(o.model.as_deref(), false).await?;
    let mut out = Vec::new();
    let report = |r: &analyst_protocol::evals::TaskResult| {
        println!(
            "  {} ({}/{} checks, {} steps, {:.0}s)",
            if r.passed { "PASS" } else { "FAIL" },
            r.checks.iter().filter(|c| c.passed).count(),
            r.checks.len(),
            r.steps,
            r.wall_s
        );
        for c in r.checks.iter().filter(|c| !c.passed) {
            println!("      FAIL {:<18} {}", c.kind, c.detail);
        }
    };
    if !analysis.is_empty() {
        app.ensure_local_tooling().await;
    }
    let launcher = vec![
        resolve_tooling_python_in(&app.global, &app.home),
        "-m".into(),
        "analyst_kernel".into(),
    ];
    let env = analyst_core::evals::EvalEnv {
        home: app.home.clone(),
        model: client,
        model_name: model_name.clone(),
        caps,
        kernel_launcher: Some(launcher),
        session_python: None,
        auto: o.auto,
    };
    for t in &analysis {
        println!("task {} ...", t.file.task.name);
        let r = analyst_core::evals::run_task(&env, t).await?;
        report(&r);
        out.push(r);
    }
    let templates = o
        .templates
        .clone()
        .unwrap_or_else(|| o.tasks.join("..").join("templates"));
    for t in &decks {
        println!("task {} (deck) ...", t.file.task.name);
        let r = deck_evals::run_task(&env, t, &templates, &app.deck_options(&o.engine)).await;
        report(&r);
        out.push(r);
    }
    let sc = analyst_core::evals::scorecard(&model_name, out);
    let path = analyst_core::evals::write_scorecard(&o.results, &sc)?;
    println!();
    print!("{}", analyst_core::evals::markdown_table(&sc));
    println!("scorecard: {}", path.display());
    Ok(if sc.tasks.iter().all(|t| t.passed) {
        0
    } else {
        1
    })
}

async fn cmd_kernel(app: &App, target: Option<String>) -> Result<i32> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let target = app.target(target.as_deref())?;
    let transport = app.transport(&target, app.remote_no_network(&target).await);
    eprintln!("spawning: {}", transport.describe());
    let spawned = transport.spawn().await?;
    let mut child = spawned.child;
    let mut stdin = spawned.stdin;
    let mut lines = BufReader::new(spawned.stdout).lines();
    let printer = tokio::spawn(async move {
        while let Ok(Some(l)) = lines.next_line().await {
            println!("{l}");
        }
    });
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(l)) = input.next_line().await {
        stdin.write_all(l.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
    }
    drop(stdin);
    let status = child.wait().await?;
    let _ = printer.await;
    Ok(status.code().unwrap_or(0))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    let code = match run(cli).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32> {
    if let Command::Init { path } = &cli.command {
        cmd_init(cli.home.clone(), path.clone())?;
        return Ok(0);
    }
    let app = App::new(&cli)?;
    match cli.command {
        Command::Init { .. } => unreachable!(),
        Command::Env { cmd, target } => cmd_env(&app, cmd, target).await.map(|_| 0),
        Command::Target { cmd } => cmd_target(&app, cmd).await.map(|_| 0),
        Command::Run {
            script,
            args,
            target,
            timeout_s,
        } => cmd_run(&app, script, args, target, timeout_s).await,
        Command::Check { paths, target } => cmd_check(&app, paths, target).await,
        Command::Eq {
            path,
            check,
            function,
            target,
        } => cmd_eq(&app, path, check, function, target).await,
        Command::Index { target } => cmd_index(&app, target).await.map(|_| 0),
        Command::Chat(args) => cmd_chat(&app, args).await.map(|_| 0),
        Command::Eval {
            cmd:
                EvalCmd::Run {
                    model,
                    tasks,
                    task,
                    auto,
                    results,
                    templates,
                    engine,
                },
        } => {
            cmd_eval(
                &app,
                EvalOpts {
                    model,
                    tasks,
                    task,
                    auto,
                    results,
                    templates,
                    engine,
                },
            )
            .await
        }
        Command::Kernel { target } => cmd_kernel(&app, target).await,
        Command::Key { cmd } => cmd_key(&app, cmd).map(|_| 0),
        Command::Deck(args) => deck::run(&app, args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn sandbox_values_are_validated() {
        for ok in ["auto", "bwrap", "sandbox-exec", "limits", "none"] {
            assert!(
                Cli::try_parse_from(["analyst", "--sandbox", ok, "index"]).is_ok(),
                "{ok}"
            );
        }
        let err = Cli::try_parse_from(["analyst", "--sandbox", "nonee", "index"])
            .err()
            .expect("typo must be rejected");
        assert!(err.to_string().contains("possible values"), "{err}");
    }

    #[test]
    fn key_set_takes_the_provider_only() {
        let cli = Cli::try_parse_from(["analyst", "key", "set", "anthropic"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Key { cmd: KeyCmd::Set { ref provider } } if provider == "anthropic"
        ));
        // The key itself is never accepted as an argument.
        assert!(Cli::try_parse_from(["analyst", "key", "set", "anthropic", "sk-123"]).is_err());
    }

    #[test]
    fn deck_commands_parse() {
        for argv in [
            vec!["analyst", "deck", "list"],
            vec![
                "analyst",
                "deck",
                "init",
                "q3",
                "--template",
                "t.potx",
                "--title",
                "Q3",
            ],
            vec![
                "analyst",
                "deck",
                "template",
                "inspect",
                "t.potx",
                "--catalog",
            ],
            vec![
                "analyst", "deck", "--deck", "q3", "refs", "add", "a.xlsx", "b.pdf",
            ],
            vec![
                "analyst", "deck", "refs", "search", "revenue", "--k", "3", "--deck", "q3",
            ],
            vec!["analyst", "deck", "render", "--out", "exports/x.pptx"],
            vec!["analyst", "deck", "check", "--engine-check", "--json"],
            vec!["analyst", "deck", "--engine", "none", "build"],
            vec!["analyst", "deck", "eject"],
            vec!["analyst", "deck", "export", "--format", "pdf"],
            vec!["analyst", "deck", "history", "list", "--limit", "5"],
            vec!["analyst", "deck", "history", "restore", "abc"],
            vec!["analyst", "deck", "history", "revert-turn", "c-1"],
            vec!["analyst", "eval", "run", "--tasks", "present/evals/tasks"],
            vec!["analyst", "chat", "--mode", "slides", "hello"],
        ] {
            assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
        }
        assert!(Cli::try_parse_from(["analyst", "deck", "--engine", "keynote", "build"]).is_err());
    }
}
