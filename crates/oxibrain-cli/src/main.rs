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
        Command::Ask { question, space } => cmd::ask::run(&dir, &question, &space).await,
        Command::EntityShow { id, space } => cmd::entity_show::run(&dir, &id, &space).await,
        Command::Timeline { entity_id, space } => {
            cmd::timeline::run(&dir, &entity_id, &space).await
        }
        Command::Why {
            statement_id,
            space,
        } => cmd::why::run(&dir, &statement_id, &space).await,
        Command::Contradictions { space } => cmd::contradictions::run(&dir, &space).await,
        Command::Reproject => cmd::reproject::run(&dir).await,
        Command::Redact {
            target,
            space,
            dry_run,
            reason,
        } => cmd::redact::run(&dir, &target, &space, dry_run, &reason).await,
        Command::Export { out } => cmd::export_cmd::run(&dir, out).await,
        Command::Import { file } => cmd::import_cmd::run(&dir, &file).await,
        Command::TokenIssue { space, caps, label } => {
            cmd::token::run_issue(&dir, &space, &caps, label.as_deref()).await
        }
        Command::TokenList => cmd::token::run_list(&dir).await,
        Command::TokenRevoke { id } => cmd::token::run_revoke(&dir, &id).await,
        Command::Serve { socket } => cmd::serve::run(&dir, socket).await,
        Command::PredicateList => cmd::predicate::run(),
        Command::Extract { episode_id, space } => {
            cmd::extract::run(&dir, &episode_id, &space).await
        }
        Command::Reextract { space } => cmd::reextract::run(&dir, &space).await,
    }
}
