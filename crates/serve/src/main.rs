//! `analyst-serve`: serves the real core to the renderer in a browser, for development and the
//! end-to-end tests. Prints `analyst-serve: listening on ws://<addr>/?token=<token>` on stdout,
//! then serves until interrupted. See docs/DEVELOPMENT.md.

use clap::Parser;
use std::io::Write;
use std::net::SocketAddr;

#[derive(Parser)]
#[command(
    name = "analyst-serve",
    about = "Development bridge: the real core over a loopback WebSocket"
)]
struct Args {
    /// Loopback address to listen on; port 0 picks a free port.
    #[arg(long, default_value = "127.0.0.1:0")]
    bind: SocketAddr,
    /// The token every connection must present (default: `$ANALYST_SERVE_TOKEN`, else random).
    #[arg(long)]
    token: Option<String>,
    /// What `dialog_pick_directory` answers (default: cancelled).
    #[arg(long)]
    pick_dir: Option<String>,
    /// What `dialog_pick_files` answers; repeat for several (default: cancelled).
    #[arg(long)]
    pick_file: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "analyst_serve=info,analyst_core=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let token = args
        .token
        .or_else(|| {
            std::env::var("ANALYST_SERVE_TOKEN")
                .ok()
                .filter(|t| !t.is_empty())
        })
        .unwrap_or_else(analyst_serve::random_token);
    let listener = analyst_serve::bind(args.bind).await?;
    let addr = listener.local_addr()?;

    // The start-up shared with the desktop shell and the CLI: home, migration, network gate,
    // configuration; `ANALYST_SANDBOX` overrides the kernel sandbox kind.
    let workbench = analyst_core::app::App::start_from_env()?.workbench()?;
    if let Some(task) = workbench.prepare_tooling() {
        tracing::info!("preparing the tooling environment ({task})");
    }
    let dialogs = analyst_serve::Dialogs {
        directory: args.pick_dir,
        files: args.pick_file,
    };
    let bridge = analyst_serve::Bridge::new(workbench.clone(), token.clone(), dialogs);

    println!("analyst-serve: listening on ws://{addr}/?token={token}");
    std::io::stdout().flush()?;

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = analyst_serve::serve(listener, bridge) => r?,
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    workbench.shutdown().await;
    Ok(())
}
