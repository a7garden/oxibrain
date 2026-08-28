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

fn resolve_space(flag: Option<&str>, home: Option<&std::path::Path>) -> anyhow::Result<String> {
    Ok(oxibrain::config::UserConfig::resolve_space(flag, home)?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout is the protocol channel in `serve --stdio` mode; diagnostics
    // must never touch it.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = Cli::parse();
    let explicit_dir = args.dir.is_some();
    let dir = args.dir.clone().unwrap_or_else(default_dir);
    let home = std::env::var_os("HOME").map(PathBuf::from);

    match args.command {
        Command::Init { space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::init::run(&dir, &space, explicit_dir, home.as_deref()).await
        }
        Command::Ingest { path, space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::ingest::run(&dir, path, &space).await
        }
        Command::Stats => cmd::stats::run(&dir).await,
        Command::Spaces => cmd::spaces::run(&dir, home.as_deref()).await,
        Command::Doctor => cmd::doctor::run(&dir).await,
        Command::Backup {
            no_projection,
            no_cache,
            out,
        } => cmd::backup::run_backup(&dir, no_projection, no_cache, out).await,
        Command::Restore { backup } => cmd::backup::run_restore(&dir, backup).await,
        Command::Ask { question, space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::ask::run(&dir, &question, &space).await
        }
        Command::Entity { command } => match command {
            cli::EntityCmd::Show { id, space } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::entity_show::run(&dir, &id, &space).await
            }
            cli::EntityCmd::Merge {
                loser,
                loser_type,
                winner,
                winner_type,
                space,
            } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::entity_merge::run(&dir, &loser, &loser_type, &winner, &winner_type, &space)
                    .await
            }
            cli::EntityCmd::Split { surface, ty, space } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::entity_split::run(&dir, &surface, &ty, &space).await
            }
            cli::EntityCmd::Alias {
                surface,
                ty,
                alias,
                space,
            } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::entity_alias::run(&dir, &surface, &ty, &alias, &space).await
            }
            cli::EntityCmd::Retract {
                statement_id,
                space,
            } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::entity_retract::run(&dir, &statement_id, &space).await
            }
        },
        Command::Timeline { entity_id, space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::timeline::run(&dir, &entity_id, &space).await
        }
        Command::Why {
            statement_id,
            space,
            dropped,
            min_confidence,
        } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            if dropped {
                cmd::why::run_dropped(&dir, &statement_id, &space, min_confidence).await
            } else {
                cmd::why::run(&dir, &statement_id, &space).await
            }
        }
        Command::Contradictions { space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::contradictions::run(&dir, &space).await
        }
        Command::Page {
            entity,
            space,
            kind,
            topic,
        } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::page::run(&dir, entity.as_deref(), &space, &kind, topic.as_deref()).await
        }
        Command::Reproject => cmd::reproject::run(&dir).await,
        Command::Redact {
            target,
            space,
            dry_run,
            reason,
        } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::redact::run(&dir, &target, &space, dry_run, &reason).await
        }
        Command::Export { out } => cmd::export_cmd::run(&dir, out).await,
        Command::Import { file } => cmd::import_cmd::run(&dir, &file).await,
        Command::ImportOxios { db, space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::import_oxios::run(&dir, &db, &space).await
        }
        Command::Token { command } => match command {
            cli::TokenCmd::Issue { space, caps, label } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::token::run_issue(&dir, &space, &caps, label.as_deref()).await
            }
            cli::TokenCmd::List => cmd::token::run_list(&dir).await,
            cli::TokenCmd::Revoke { id } => cmd::token::run_revoke(&dir, &id).await,
        },
        Command::Serve {
            stdio,
            http,
            ui_dir,
        } => {
            // stdio is the default transport; the flag exists so the
            // canonical `serve --stdio` spelling is explicit.
            let _ = stdio;
            cmd::serve::run(&dir, http, ui_dir, home.as_deref()).await
        }
        Command::Predicate { command } => match command {
            cli::PredicateCmd::List => cmd::predicate::run(),
            cli::PredicateCmd::Add { json, space } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::predicate::run_add(&dir, &json, &space).await
            }
        },
        Command::Declare { json, space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::declare::run(&dir, &json, &space).await
        }
        Command::Source { command } => match command {
            cli::SourceCmd::Policy {
                name,
                trust,
                effective_from,
                effective_to,
                space,
            } => {
                let space = resolve_space(space.as_deref(), home.as_deref())?;
                cmd::source_policy::run(&dir, &name, &trust, effective_from, effective_to, &space)
                    .await
            }
        },
        Command::Extract { pending, limit } => {
            debug_assert!(pending, "clap enforces --pending (required = true)");
            cmd::extract::run(&dir, limit).await
        }
        Command::Index { documents, embed } => cmd::index::run(&dir, documents, embed).await,
        Command::Reextract { space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::reextract::run(&dir, &space).await
        }
        Command::Model { command } => cmd::model::run(&command).await,
        Command::Eval { suite } => cmd::eval::run(&suite).await,
        Command::Space { command } => match command {
            cli::SpaceCmd::Add { name } => {
                cmd::space_add::run(&dir, explicit_dir, home.as_deref(), &name).await
            }
            cli::SpaceCmd::Default { name } => {
                cmd::space_default::run(&dir, home.as_deref(), name.as_deref()).await
            }
            cli::SpaceCmd::Remove { name, purge } => {
                cmd::space_remove::run(&dir, home.as_deref(), &name, purge).await
            }
        },
    }
}
