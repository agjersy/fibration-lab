//! `present`: the headless harness for phase 0 and evaluations (BUILD-SPEC W13).

use analyst_deck::agent::tools::Mode;
use analyst_deck::agent::{ApprovalDecision, ApprovalRequest, Approver, AutoApprover};
use analyst_deck::config::{default_home, load_global};
use analyst_deck::project::Project;
use analyst_deck::session::{build_agent, policy_for, resolve_model};
use analyst_deck::workspace::{OpenOptions, Workspace};
use analyst_deck::{evals, refs};
use analyst_deck_protocol::deck::DeckMode;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "present",
    version,
    about = "Personal Present: build presentations with an agent, headless"
)]
struct Cli {
    /// Present home directory (default: PRESENT_HOME, %APPDATA%\Present, or ~/.present).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Project directory (default: the current directory).
    #[arg(long, short = 'C', global = true)]
    project: Option<PathBuf>,
    /// Fidelity engine: auto, powerpoint, libreoffice, none.
    #[arg(long, global = true, default_value = "auto")]
    engine: String,
    /// Code sandbox: auto, native, limits.
    #[arg(long, global = true, default_value = "auto")]
    sandbox: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a project in a directory from a template (.pptx or .potx).
    Init {
        dir: PathBuf,
        #[arg(long)]
        template: PathBuf,
        #[arg(long)]
        name: Option<String>,
        /// Start in code generation mode.
        #[arg(long)]
        code: bool,
    },
    /// Template tools.
    Template {
        #[command(subcommand)]
        command: TemplateCommand,
    },
    /// Reference tools.
    Refs {
        #[command(subcommand)]
        command: RefsCommand,
    },
    /// Render the deck to a .pptx.
    Render {
        #[arg(long, default_value = "exports/deck.pptx")]
        out: String,
    },
    /// Run the deck checker.
    Check {
        /// Also convert with the fidelity engine.
        #[arg(long)]
        engine_check: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run the build script (code mode).
    Build,
    /// Switch the project to code generation mode.
    Eject,
    /// Export the deck: pptx, pdf, png, md, notes-docx.
    Export {
        #[arg(long, default_value = "pptx")]
        format: String,
    },
    /// Snapshot history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Send one message to the agent.
    Chat {
        text: String,
        /// provider/model (default: [defaults] model in config.toml).
        #[arg(long, short)]
        model: Option<String>,
        #[arg(long, default_value = "build")]
        mode: String,
        /// Conversation id (default: "main").
        #[arg(long, default_value = "main")]
        conversation: String,
        /// Approve everything the policy asks about without prompting.
        #[arg(long)]
        auto: bool,
    },
    /// Model tools.
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    /// Evaluation suite.
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
}

#[derive(Subcommand)]
enum TemplateCommand {
    /// Print the template description of a .pptx or .potx file.
    Inspect {
        file: PathBuf,
        /// Print the compact layout catalog the agent sees instead of the full description.
        #[arg(long)]
        catalog: bool,
    },
}

#[derive(Subcommand)]
enum RefsCommand {
    /// Copy files into the project and index them.
    Add { files: Vec<PathBuf> },
    /// List the project's references.
    List,
    /// Search the references.
    Search {
        query: String,
        #[arg(long, default_value_t = 8)]
        k: usize,
    },
    /// Remove a reference.
    Remove { ref_id: String },
}

#[derive(Subcommand)]
enum HistoryCommand {
    List,
    Restore { snapshot_id: String },
    RevertTurn { turn: String },
}

#[derive(Subcommand)]
enum ModelsCommand {
    /// Probe a model's capabilities and print the record.
    Probe { model: Option<String> },
}

#[derive(Subcommand)]
enum EvalCommand {
    Run {
        #[arg(long, short)]
        model: Option<String>,
        #[arg(long, default_value = "evals/tasks")]
        tasks: PathBuf,
        #[arg(long, default_value = "evals/templates")]
        templates: PathBuf,
        #[arg(long)]
        task: Option<String>,
        #[arg(long, default_value = "evals/results")]
        results: PathBuf,
        /// Keep the working projects in this directory instead of a temporary one.
        #[arg(long)]
        work: Option<PathBuf>,
    },
}

/// Asks on the terminal for each proposal.
struct TerminalApprover;

#[async_trait]
impl Approver for TerminalApprover {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        println!(
            "\n── {} ({}) ──\n{}",
            request.summary, request.reason, request.diff
        );
        print!("accept? [y]es / [n]o / text = reject with a note: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return ApprovalDecision::Reject { note: None };
        }
        match line.trim() {
            "y" | "Y" | "yes" | "" => ApprovalDecision::Accept,
            "n" | "N" | "no" => ApprovalDecision::Reject { note: None },
            note => ApprovalDecision::Reject {
                note: Some(note.to_string()),
            },
        }
    }
}

fn print_diags(diags: &[Value]) {
    for d in diags {
        let place = [d["slide_id"].as_str(), d["element_id"].as_str()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "{:<5} {:<20} {:<22} {}",
            d["severity"].as_str().unwrap_or(""),
            d["rule"].as_str().unwrap_or(""),
            place,
            d["message"].as_str().unwrap_or("")
        );
    }
    let count = |sev: &str| diags.iter().filter(|d| d["severity"] == sev).count();
    println!("{} errors, {} warnings", count("error"), count("warn"));
}

async fn open(cli: &Cli, home: &Path) -> Result<Workspace> {
    let root = cli.project.clone().unwrap_or(std::env::current_dir()?);
    let global = load_global(home)?;
    let mut options = OpenOptions::new(home);
    options.engine = if cli.engine == "auto" {
        serde_json::to_value(global.render.engine)?
            .as_str()
            .unwrap_or("auto")
            .to_string()
    } else {
        cli.engine.clone()
    };
    options.sandbox = cli.sandbox.clone();
    Workspace::open(&root, &options)
        .await
        .with_context(|| format!("opening the project at {}", root.display()))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PRESENT_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match run(&cli).await {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

async fn run(cli: &Cli) -> Result<i32> {
    let home = cli.home.clone().unwrap_or_else(default_home);
    std::fs::create_dir_all(&home)?;
    // Every network decision from here on is recorded in `<home>/egress.jsonl`.
    wb_net::init(&home);
    match &cli.command {
        Command::Init {
            dir,
            template,
            name,
            code,
        } => {
            let name = name.clone().unwrap_or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "deck".into())
            });
            let mode = if *code {
                DeckMode::Code
            } else {
                DeckMode::Structured
            };
            let project = Project::create(dir, &name, template, mode)?;
            if *code {
                let script = "from present_sdk import Deck\n\n\ndef main():\n    deck = Deck.open_template()\n    deck.save()\n\n\nif __name__ == \"__main__\":\n    main()\n";
                std::fs::write(project.root.join("deck/build.py"), script)?;
            }
            println!("created {}", project.root.display());
            Ok(0)
        }
        Command::Template {
            command: TemplateCommand::Inspect { file, catalog },
        } => {
            // Inspect through a throwaway project so the kernel's path rules hold.
            let tmp = std::env::temp_dir().join(format!("present-inspect-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let project = Project::create(&tmp, "inspect", file, DeckMode::Structured)?;
            let mut options = OpenOptions::new(&home);
            options.engine = "none".into();
            let ws = Workspace::open(&project.root, &options).await?;
            let rel = ws.deck().read_deck()?.template;
            let desc = ws
                .kernel
                .call_value("template.inspect", json!({"path": rel}))
                .await;
            ws.close().await;
            let _ = std::fs::remove_dir_all(&tmp);
            let desc = desc?;
            let shown = if *catalog {
                analyst_deck::agent::dispatch::layout_catalog(&desc)
            } else {
                desc
            };
            println!("{}", serde_json::to_string_pretty(&shown)?);
            Ok(0)
        }
        Command::Refs { command } => {
            let ws = open(cli, &home).await?;
            let outcome = async {
                match command {
                    RefsCommand::Add { files } => {
                        for file in files {
                            let (entry, warnings) = ws
                                .add_reference(file)
                                .await
                                .with_context(|| format!("adding {}", file.display()))?;
                            println!("{}  {}  {}", entry.id, entry.version, entry.title);
                            for w in warnings {
                                println!("    warning: {}", w["message"].as_str().unwrap_or(""));
                            }
                        }
                    }
                    RefsCommand::List => {
                        for r in refs::load_manifest(&ws.project.root)?.refs {
                            println!(
                                "{}  {}  {:<10} {}",
                                r.id,
                                r.version,
                                serde_json::to_value(r.kind)?.as_str().unwrap_or(""),
                                r.title
                            );
                        }
                    }
                    RefsCommand::Search { query, k } => {
                        for hit in ws.search(query, None, *k).await? {
                            println!(
                                "{}\n    {} — {}\n    {}",
                                hit.cite(),
                                hit.title,
                                hit.heading_path.join(" > "),
                                hit.snippet
                            );
                        }
                    }
                    RefsCommand::Remove { ref_id } => {
                        ws.remove_reference(
                            &ref_id
                                .parse()
                                .map_err(|_| anyhow!("not a reference id: {ref_id}"))?,
                        )?;
                        println!("removed {ref_id}");
                    }
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            ws.close().await;
            outcome.map(|_| 0)
        }
        Command::Render { out } => {
            let ws = open(cli, &home).await?;
            let built = ws.build(out, "", None).await;
            ws.close().await;
            let built = built?;
            println!(
                "{}  ({} elements, sha256 {})",
                out,
                built["element_map"].as_array().map(Vec::len).unwrap_or(0),
                built["sha256"].as_str().unwrap_or("")
            );
            Ok(0)
        }
        Command::Check { engine_check, json } => {
            let ws = open(cli, &home).await?;
            let outcome = async {
                let out = ".present/cache/check.pptx";
                ws.build(out, "", None).await?;
                Ok::<_, anyhow::Error>(ws.check(out, None, None, *engine_check).await?)
            }
            .await;
            ws.close().await;
            let diags = outcome?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&diags)?);
            } else {
                print_diags(&diags);
            }
            Ok(if diags.iter().any(|d| d["severity"] == "error") {
                2
            } else {
                0
            })
        }
        Command::Build => {
            let ws = open(cli, &home).await?;
            let built = ws.build("exports/deck.pptx", "", None).await;
            let kernel_network = ws.kernel.network_isolation().label();
            ws.close().await;
            match built {
                Ok(v) => {
                    print!("{}", v["stdout"].as_str().unwrap_or(""));
                    println!(
                        "built exports/deck.pptx (sandbox: {}, kernel network: {kernel_network})",
                        v["sandbox"].as_str().unwrap_or("n/a")
                    );
                    Ok(0)
                }
                Err(e) => {
                    for key in ["stdout", "stderr"] {
                        if let Some(text) = e.data(key).and_then(|v| v.as_str()) {
                            eprint!("{text}");
                        }
                    }
                    Err(e.into())
                }
            }
        }
        Command::Eject => {
            let ws = open(cli, &home).await?;
            let mut state = analyst_deck::agent::dispatch::ToolState::default();
            let change = analyst_deck::agent::dispatch::Change::ModeSwitch {
                reason: "requested from the command line".into(),
            };
            let result = analyst_deck::agent::dispatch::apply(&ws, &mut state, &change).await;
            ws.close().await;
            let out = result.map_err(|e| anyhow!(e))?;
            println!(
                "the project is now in code generation mode: {}",
                out["script"].as_str().unwrap_or("deck/build.py")
            );
            Ok(0)
        }
        Command::Export { format } => {
            let ws = open(cli, &home).await?;
            let exported = ws.export(format, None).await;
            ws.close().await;
            let e = exported?;
            println!(
                "{}  (snapshot {}, {} errors, {} warnings{})",
                e.path,
                e.snapshot_id,
                e.errors,
                e.warnings,
                e.engine
                    .map(|x| format!(", engine {x}"))
                    .unwrap_or_default()
            );
            Ok(0)
        }
        Command::History { command } => {
            let root = cli.project.clone().unwrap_or(std::env::current_dir()?);
            let project = Project::open(&root)?;
            let snaps = analyst_deck::snapshots::SnapshotStore::new(&project.root);
            match command {
                HistoryCommand::List => {
                    for r in snaps.log()? {
                        println!(
                            "{}  {}  {:<11} {}{}",
                            r.snapshot_id,
                            r.ts,
                            serde_json::to_value(r.reason)?.as_str().unwrap_or(""),
                            r.turn_id.unwrap_or_default(),
                            r.label.map(|l| format!("  {l}")).unwrap_or_default()
                        );
                    }
                }
                HistoryCommand::Restore { snapshot_id } => {
                    let id = snaps.restore(
                        &snapshot_id
                            .parse()
                            .map_err(|_| anyhow!("not a snapshot id: {snapshot_id}"))?,
                    )?;
                    println!("restored {snapshot_id} as {id}");
                }
                HistoryCommand::RevertTurn { turn } => {
                    let id = snaps.revert_turn(turn)?;
                    println!("reverted turn {turn} (snapshot {id})");
                }
            }
            Ok(0)
        }
        Command::Chat {
            text,
            model,
            mode,
            conversation,
            auto,
        } => {
            let global = load_global(&home)?;
            let resolved = resolve_model(&home, &global, model.as_deref(), false).await?;
            if resolved.caps.degraded {
                eprintln!(
                    "note: {} is below the capability baseline ({}); running in the degraded tier",
                    resolved.name,
                    resolved.caps.degraded_reasons.join(", ")
                );
            }
            let ws = Arc::new(open(cli, &home).await?);
            let outcome = async {
                let policy = policy_for(&ws.project.config()?)?;
                let approver: Arc<dyn Approver> = if *auto {
                    Arc::new(AutoApprover)
                } else {
                    Arc::new(TerminalApprover)
                };
                let mode: Mode = mode.parse().map_err(|e: String| anyhow!(e))?;
                let max_steps = policy.max_steps;
                let mut agent = build_agent(
                    ws.clone(),
                    &resolved,
                    policy,
                    approver,
                    mode,
                    conversation,
                    max_steps,
                )?;
                agent.on_text = Some(Arc::new(|delta: &str| {
                    print!("{delta}");
                    let _ = std::io::stdout().flush();
                }));
                agent.on_event = Some(Arc::new(|event: &wb_transcript::Event| {
                    if let wb_transcript::Event::ToolCall { name, .. } = event {
                        eprintln!("\n[{name}]");
                    }
                }));
                agent.run_turn(text).await
            }
            .await;
            ws.close().await;
            let summary = outcome?;
            println!(
                "\n\nturn {}: {} steps, {} tool calls, slides changed: {}",
                summary.turn_id,
                summary.steps,
                summary.tool_calls,
                if summary.slides_changed.is_empty() {
                    "none".into()
                } else {
                    summary.slides_changed.join(", ")
                }
            );
            for issue in &summary.open_issues {
                println!("open issue: {issue}");
            }
            Ok(0)
        }
        Command::Models {
            command: ModelsCommand::Probe { model },
        } => {
            let global = load_global(&home)?;
            let resolved = resolve_model(&home, &global, model.as_deref(), true).await?;
            println!(
                "{}\n{}",
                resolved.name,
                serde_json::to_string_pretty(&resolved.caps)?
            );
            Ok(0)
        }
        Command::Eval {
            command:
                EvalCommand::Run {
                    model,
                    tasks,
                    templates,
                    task,
                    results,
                    work,
                },
        } => {
            let global = load_global(&home)?;
            let resolved = resolve_model(&home, &global, model.as_deref(), false).await?;
            let loaded = evals::load_tasks(tasks, task.as_deref())?;
            let tmp;
            let work_dir = match work {
                Some(dir) => dir.clone(),
                None => {
                    tmp = std::env::temp_dir().join(format!(
                        "present-eval-{}-{}",
                        std::process::id(),
                        chrono::Utc::now().timestamp()
                    ));
                    tmp.clone()
                }
            };
            std::fs::create_dir_all(&work_dir)?;
            let mut options = OpenOptions::new(&home);
            options.engine = cli.engine.clone();
            options.sandbox = cli.sandbox.clone();
            let mut done = Vec::new();
            for t in &loaded {
                eprintln!("running {} ...", t.file.task.name);
                let _ = std::fs::remove_dir_all(work_dir.join(&t.file.task.name));
                done.push(evals::run_task(t, &resolved, templates, &work_dir, &options).await);
            }
            let card = evals::scorecard(&resolved.name, done);
            print!("{}", evals::render_scorecard(&card));
            let path = evals::write_scorecard(results, &card)?;
            println!("scorecard: {}", path.display());
            Ok(if card.tasks.iter().all(|t| t.passed) {
                0
            } else {
                3
            })
        }
    }
}
