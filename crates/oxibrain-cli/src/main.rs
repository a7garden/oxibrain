mod cli;
mod cmd;

use clap::Parser;
use cli::{Cli, Command};
use std::path::PathBuf;

fn default_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".oxi").join("brain")
    } else {
        PathBuf::from(".oxibrain")
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = Cli::parse();
    let dir = args.dir.clone().unwrap_or_else(default_dir);

    match args.command {
        Command::Init { space } => cmd::init::run(&dir, &space).await,
        Command::Ingest { path, space } => cmd::ingest::run(&dir, path, &space).await,
        Command::Stats => cmd::stats::run(&dir).await,
        Command::Doctor => cmd::doctor::run(&dir).await,
        Command::Backup {
            no_projection,
            no_cache,
            out,
        } => cmd::backup::run_backup(&dir, no_projection, no_cache, out).await,
        Command::Restore { backup } => cmd::backup::run_restore(&dir, backup).await,
    }
}
