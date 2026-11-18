//! `analyst deck ...`: the decks of the repository (`decks/<slug>/`), headless. Every command
//! opens the deck through the same per-deck service the desktop app uses; its kernel runs
//! without network. History is the repository's git checkpoints (code and decks together).

use crate::App;
use analyst_core::service::decks::{self, DeckOptions, DeckServices};
use analyst_core::service::history;
use analyst_core::target::Target;
use analyst_deck::service::DeckService;
use analyst_deck_protocol::deck::DeckMode;
use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Args)]
pub struct DeckArgs {
    /// The deck: its folder name under `decks/` (default: the only deck of the repository).
    #[arg(long, global = true)]
    deck: Option<String>,
    /// Rendering engine for the deck kernel: auto, powerpoint, libreoffice, none.
    #[arg(
        long,
        global = true,
        default_value = "auto",
        value_parser = ["auto", "powerpoint", "libreoffice", "none"]
    )]
    engine: String,
    #[command(subcommand)]
    cmd: DeckCmd,
}

#[derive(Subcommand)]
enum DeckCmd {
    /// List the decks of the repository.
    List,
    /// Create a deck under `decks/<slug>/` from a template (.pptx or .potx). A directory that
    /// is not a project yet gets the project template (without a venv) and a git repository.
    Init {
        slug: String,
        #[arg(long)]
        template: PathBuf,
        /// Deck title (default: the slug).
        #[arg(long)]
        title: Option<String>,
        /// Start in code generation mode.
        #[arg(long)]
        code: bool,
    },
    /// Template tools.
    Template {
        #[command(subcommand)]
        cmd: TemplateCmd,
    },
    /// The deck's references.
    Refs {
        #[command(subcommand)]
        cmd: RefsCmd,
    },
    /// Render the deck to a .pptx (path relative to the deck folder).
    Render {
        #[arg(long, default_value = "exports/deck.pptx")]
        out: String,
    },
    /// Run the deck checker; exits 2 when it reports errors.
    Check {
        /// Also convert with the fidelity engine.
        #[arg(long)]
        engine_check: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run the build script (code mode) in the sandbox.
    Build,
    /// Switch the deck to code generation mode (checkpointed first).
    Eject,
    /// Export the deck: pptx, pdf, png, md, notes-docx (checkpointed first).
    Export {
        #[arg(long, default_value = "pptx")]
        format: String,
    },
    /// The undo timeline: the repository's git checkpoints, which cover code and decks.
    History {
        #[command(subcommand)]
        cmd: HistoryCmd,
    },
}

#[derive(Subcommand)]
enum TemplateCmd {
    /// Print the template description of a .pptx or .potx file.
    Inspect {
        file: PathBuf,
        /// Print the compact layout catalog the agent sees instead of the full description.
        #[arg(long)]
        catalog: bool,
    },
}

#[derive(Subcommand)]
enum RefsCmd {
    /// Copy files into the deck's references and index them.
    Add { files: Vec<PathBuf> },
    /// List the references.
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
enum HistoryCmd {
    /// Checkpoints, newest first.
    List {
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Restore a checkpoint (the current state is checkpointed first).
    Restore { checkpoint: String },
    /// Undo a turn (`<conversation>-<n>`, as `analyst chat` prints it).
    RevertTurn { turn: String },
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

impl App {
    /// How deck kernels are started from the command line: no provider keys in their
    /// environment, the chosen engine.
    pub fn deck_options(&self, engine: &str) -> DeckOptions {
        DeckOptions {
            engine: engine.to_string(),
            env_remove: analyst_core::service::terminal::key_env_names(&self.global),
            ..DeckOptions::default()
        }
    }

    fn deck_session(&self) -> Result<analyst_core::session::Session> {
        self.session(
            &Target::Local,
            self.global.defaults.model.as_deref().unwrap_or("-"),
        )
    }
}

/// The deck's service; the caller shuts the services down when done.
async fn open(app: &App, args: &DeckArgs) -> Result<(Arc<DeckServices>, Arc<DeckService>)> {
    let session = app.deck_session()?;
    let services = DeckServices::new(&app.repo, &app.home, &session.dir);
    services.set_options(app.deck_options(&args.engine));
    let svc = services
        .resolve(args.deck.as_deref())
        .await
        .map_err(|e| anyhow!("{}", e.message))?;
    let repo = app.repo.clone();
    svc.ws.set_tree_identity(Arc::new(move || {
        analyst_core::service::artifacts::current_tree(&repo)
    }));
    Ok((services, svc))
}

pub async fn run(app: &App, args: DeckArgs) -> Result<i32> {
    match &args.cmd {
        DeckCmd::List => {
            for d in decks::list(&app.repo) {
                println!(
                    "{}  {:<10} {} slides  {}  ({})",
                    d.slug, d.mode, d.slides, d.title, d.template
                );
            }
            Ok(0)
        }
        DeckCmd::Init {
            slug,
            template,
            title,
            code,
        } => {
            std::fs::create_dir_all(&app.repo)
                .with_context(|| format!("creating {}", app.repo.display()))?;
            if !analyst_core::config::project_config_path(&app.repo).is_file()
                || !app.repo.join(".git").exists()
            {
                let opts = analyst_core::init::InitOptions {
                    run_uv: false,
                    git: true,
                };
                analyst_core::init::init_repo(&app.repo, &opts)?;
            }
            let template = std::fs::canonicalize(template)
                .with_context(|| format!("template {}", template.display()))?;
            let mode = if *code {
                DeckMode::Code
            } else {
                DeckMode::Structured
            };
            let title = title.clone().unwrap_or_else(|| slug.clone());
            let d = decks::create(&app.repo, slug, &title, &template, mode)
                .map_err(|e| anyhow!("{}", e.message))?;
            let root = decks::deck_root(&app.repo, slug);
            if *code {
                let script = "from present_sdk import Deck\n\n\ndef main():\n    deck = Deck.open_template()\n    deck.save()\n\n\nif __name__ == \"__main__\":\n    main()\n";
                std::fs::write(root.join("deck/build.py"), script)?;
            }
            println!("created {} ({})", decks::deck_rel(&d.slug), d.title);
            Ok(0)
        }
        DeckCmd::Template {
            cmd: TemplateCmd::Inspect { file, catalog },
        } => template_inspect(app, &args, file, *catalog).await,
        DeckCmd::History { cmd } => history_cmd(app, cmd).await,
        _ => {
            let (services, svc) = open(app, &args).await?;
            let outcome = deck_cmd(&svc, &args.cmd).await;
            services.shutdown().await;
            outcome
        }
    }
}

async fn template_inspect(
    app: &App,
    args: &DeckArgs,
    file: &PathBuf,
    catalog: bool,
) -> Result<i32> {
    // Inspect through a throwaway deck so the kernel's path rules hold.
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("inspect");
    let file = std::fs::canonicalize(file).with_context(|| format!("{}", file.display()))?;
    let project =
        analyst_deck::project::Project::create(&root, "inspect", &file, DeckMode::Structured)?;
    let mut options =
        analyst_deck::workspace::OpenOptions::new(&app.home, &tmp.path().join("index.sqlite"));
    options.engine = "none".into();
    options.env_remove = app.deck_options(&args.engine).env_remove;
    let ws = analyst_deck::workspace::Workspace::open(&project.root, &options).await?;
    let rel = ws.deck().read_deck()?.template;
    let desc = ws
        .kernel
        .call_value("template.inspect", json!({"path": rel}))
        .await;
    ws.close().await;
    let desc = desc?;
    let shown = if catalog {
        analyst_deck::tools::dispatch::layout_catalog(&desc)
    } else {
        desc
    };
    println!("{}", serde_json::to_string_pretty(&shown)?);
    Ok(0)
}

async fn history_cmd(app: &App, cmd: &HistoryCmd) -> Result<i32> {
    let session = app.deck_session()?;
    match cmd {
        HistoryCmd::List { limit } => {
            for c in
                history::list(&app.repo, &session, *limit).map_err(|e| anyhow!("{}", e.message))?
            {
                println!(
                    "{}  {}  {}{}",
                    c.id,
                    c.created_at,
                    c.label.unwrap_or_else(|| "-".into()),
                    c.turn.map(|t| format!("  turn {t}")).unwrap_or_default()
                );
            }
        }
        HistoryCmd::Restore { checkpoint } => {
            let safety =
                history::restore(&app.repo, checkpoint).map_err(|e| anyhow!("{}", e.message))?;
            println!("restored {checkpoint} (the replaced state is checkpoint {safety})");
        }
        HistoryCmd::RevertTurn { turn } => {
            let id = history::revert_turn(&app.repo, &session, turn)
                .map_err(|e| anyhow!("{}", e.message))?;
            println!("reverted turn {turn} (restored checkpoint {id})");
        }
    }
    Ok(0)
}

async fn deck_cmd(svc: &Arc<DeckService>, cmd: &DeckCmd) -> Result<i32> {
    let ws = &svc.ws;
    match cmd {
        DeckCmd::Refs { cmd } => {
            match cmd {
                RefsCmd::Add { files } => {
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
                RefsCmd::List => {
                    for r in svc.refs_list().map_err(|e| anyhow!("{}", e.message))? {
                        println!(
                            "{}  {}  {:<10} {}",
                            r.id,
                            r.version,
                            serde_json::to_value(r.kind)?.as_str().unwrap_or(""),
                            r.title
                        );
                    }
                }
                RefsCmd::Search { query, k } => {
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
                RefsCmd::Remove { ref_id } => {
                    svc.refs_remove(ref_id)
                        .map_err(|e| anyhow!("{}", e.message))?;
                    println!("removed {ref_id}");
                }
            }
            Ok(0)
        }
        DeckCmd::Render { out } => {
            let built = ws.build(out, "", None).await?;
            println!(
                "{}  ({} elements, sha256 {})",
                out,
                built["element_map"].as_array().map(Vec::len).unwrap_or(0),
                built["sha256"].as_str().unwrap_or("")
            );
            Ok(0)
        }
        DeckCmd::Check { engine_check, json } => {
            let out = ".present/cache/check.pptx";
            ws.build(out, "", None).await?;
            let diags = ws.check(out, None, None, *engine_check).await?;
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
        DeckCmd::Build => match ws.build("exports/deck.pptx", "", None).await {
            Ok(v) => {
                print!("{}", v["stdout"].as_str().unwrap_or(""));
                println!(
                    "built exports/deck.pptx (sandbox: {}, kernel network: {})",
                    v["sandbox"].as_str().unwrap_or("n/a"),
                    ws.kernel.network_isolation().label()
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
        },
        DeckCmd::Eject => {
            let out = svc
                .switch_to_code()
                .await
                .map_err(|e| anyhow!("{}", e.message))?;
            println!(
                "the deck is now in code generation mode: {}",
                out["script"].as_str().unwrap_or("deck/build.py")
            );
            Ok(0)
        }
        DeckCmd::Export { format } => {
            let e = svc
                .export(format)
                .await
                .map_err(|e| anyhow!("{}", e.message))?;
            println!(
                "{}  (checkpoint {}, {} errors, {} warnings{})",
                e.path,
                e.checkpoint,
                e.errors,
                e.warnings,
                e.engine
                    .map(|x| format!(", engine {x}"))
                    .unwrap_or_default()
            );
            Ok(0)
        }
        DeckCmd::List
        | DeckCmd::Init { .. }
        | DeckCmd::Template { .. }
        | DeckCmd::History { .. } => {
            unreachable!("handled without opening the deck")
        }
    }
}
