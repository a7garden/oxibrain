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
        Command::Sync { path, space } => cmd::sync::run(&dir, &path, &space).await,
        Command::Doctor => cmd::doctor::run(&dir).await,
        Command::Backup {
            no_projection,
            no_cache,
            out,
        } => cmd::backup::run_backup(&dir, no_projection, no_cache, out).await,
        Command::Restore { backup } => cmd::backup::run_restore(&dir, backup).await,
        Command::Ask { question, space } => cmd::ask::run(&dir, &question, &space).await,
        Command::Entity { command } => match command {
            cli::EntityCmd::Show { id, space } => cmd::entity_show::run(&dir, &id, &space).await,
            cli::EntityCmd::Merge {
                loser,
                loser_type,
                winner,
                winner_type,
                space,
            } => {
                cmd::entity_merge::run(&dir, &loser, &loser_type, &winner, &winner_type, &space)
                    .await
            }
            cli::EntityCmd::Split { surface, ty, space } => {
                cmd::entity_split::run(&dir, &surface, &ty, &space).await
            }
            cli::EntityCmd::Alias {
                surface,
                ty,
                alias,
                space,
            } => cmd::entity_alias::run(&dir, &surface, &ty, &alias, &space).await,
            cli::EntityCmd::Retract {
                statement_id,
                space,
            } => cmd::entity_retract::run(&dir, &statement_id, &space).await,
        },
        Command::Timeline { entity_id, space } => {
            cmd::timeline::run(&dir, &entity_id, &space).await
        }
        Command::Why {
            statement_id,
            space,
            dropped,
            min_confidence,
        } => {
            if dropped {
                cmd::why::run_dropped(&dir, &statement_id, &space, min_confidence).await
            } else {
                cmd::why::run(&dir, &statement_id, &space).await
            }
        }
        Command::Contradictions { space } => cmd::contradictions::run(&dir, &space).await,
        Command::Page {
            entity,
            space,
            kind,
            topic,
        } => cmd::page::run(&dir, entity.as_deref(), &space, &kind, topic.as_deref()).await,
        Command::Reproject => cmd::reproject::run(&dir).await,
        Command::Redact {
            target,
            space,
            dry_run,
            reason,
        } => cmd::redact::run(&dir, &target, &space, dry_run, &reason).await,
        Command::Export { out } => cmd::export_cmd::run(&dir, out).await,
        Command::Import { file } => cmd::import_cmd::run(&dir, &file).await,
        Command::ImportOxios { db, space } => cmd::import_oxios::run(&dir, &db, &space).await,
        Command::Token { command } => match command {
            cli::TokenCmd::Issue { space, caps, label } => {
                cmd::token::run_issue(&dir, &space, &caps, label.as_deref()).await
            }
            cli::TokenCmd::List => cmd::token::run_list(&dir).await,
            cli::TokenCmd::Revoke { id } => cmd::token::run_revoke(&dir, &id).await,
        },
        Command::Serve {
            socket,
            http,
            require_token,
            daemon,
            ui_dir,
        } => cmd::serve::run(&dir, socket, http, require_token, daemon, ui_dir).await,
        Command::Predicate { command } => match command {
            cli::PredicateCmd::List => cmd::predicate::run(),
            cli::PredicateCmd::Add { json, space } => {
                cmd::predicate::run_add(&dir, &json, &space).await
            }
        },
        Command::Declare { json, space } => cmd::declare::run(&dir, &json, &space).await,
        Command::Source { command } => match command {
            cli::SourceCmd::Policy {
                name,
                trust,
                effective_from,
                effective_to,
                space,
            } => {
                cmd::source_policy::run(&dir, &name, &trust, effective_from, effective_to, &space)
                    .await
            }
        },
        Command::Extract { episode_id, space } => {
            cmd::extract::run(&dir, &episode_id, &space).await
        }
        Command::Reextract { space } => cmd::reextract::run(&dir, &space).await,
        Command::Model { command } => cmd::model::run(&command).await,
        Command::Eval { suite } => cmd::eval::run(&suite).await,
    }
}
