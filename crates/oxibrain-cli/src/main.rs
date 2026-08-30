mod cli;
mod cmd;
mod op;

use clap::Parser;
use cli::{AdminCmd, Cli, Command};
use std::path::PathBuf;

fn default_dir() -> PathBuf {
    oxibrain::paths::brain_dir()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout is the protocol channel for the op surface and `serve --stdio`
    // mode; diagnostics must never touch it.
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
        // Agent-first op surface (spec agent-first-cli-v1; ADR-012/013).
        // These print one JSON envelope to stdout and exit with the
        // envelope's code — anyhow's error path is not involved.
        Command::Describe => {
            let code = op::describe(&dir).await;
            std::process::exit(code);
        }
        Command::Schema { op } => {
            let code = op::schema(op.as_deref());
            std::process::exit(code);
        }
        Command::Op(args) => {
            let code = op::run(&dir, &args).await;
            std::process::exit(code);
        }

        // Machine/product lifecycle (spec §10) — not the agent surface.
        Command::Admin { command } => match command {
            AdminCmd::Init { space } => {
                // Creation default (deterministic, reads no config) — not
                // resolution (ADR-013).
                let space = space.unwrap_or_else(|| "personal".to_string());
                cmd::init::run(&dir, &space, explicit_dir, home.as_deref()).await
            }
            AdminCmd::Stats => cmd::stats::run(&dir).await,
            AdminCmd::Spaces => cmd::spaces::run(&dir).await,
            AdminCmd::Doctor => cmd::doctor::run(&dir).await,
            AdminCmd::Backup {
                no_projection,
                no_cache,
                out,
            } => cmd::backup::run_backup(&dir, no_projection, no_cache, out).await,
            AdminCmd::Restore { backup } => cmd::backup::run_restore(&dir, backup).await,
            AdminCmd::EntitySplit { surface, ty, space } => {
                cmd::entity_split::run(&dir, &surface, &ty, &space).await
            }
            AdminCmd::Reproject => cmd::reproject::run(&dir).await,
            AdminCmd::Export { out } => cmd::export_cmd::run(&dir, out).await,
            AdminCmd::Import { file } => cmd::import_cmd::run(&dir, &file).await,
            AdminCmd::ImportOxios { db, space } => cmd::import_oxios::run(&dir, &db, &space).await,
            AdminCmd::Token { command } => match command {
                cli::TokenCmd::Issue { space, caps, label } => {
                    cmd::token::run_issue(&dir, &space, &caps, label.as_deref()).await
                }
                cli::TokenCmd::List => cmd::token::run_list(&dir).await,
                cli::TokenCmd::Revoke { id } => cmd::token::run_revoke(&dir, &id).await,
            },
            AdminCmd::Serve {
                stdio,
                http,
                ui_dir,
            } => {
                // stdio is the default transport; the flag exists so the
                // canonical `serve --stdio` spelling is explicit.
                let _ = stdio;
                cmd::serve::run(&dir, http, ui_dir).await
            }
            AdminCmd::Predicate { command } => match command {
                cli::PredicateCmd::List => cmd::predicate::run(&dir).await,
                cli::PredicateCmd::Add { json, space } => {
                    cmd::predicate::run_add(&dir, &json, &space).await
                }
            },
            AdminCmd::Source { command } => match command {
                cli::SourceCmd::Policy {
                    name,
                    trust,
                    effective_from,
                    effective_to,
                    space,
                } => {
                    cmd::source_policy::run(
                        &dir,
                        &name,
                        &trust,
                        effective_from,
                        effective_to,
                        &space,
                    )
                    .await
                }
            },
            AdminCmd::Extract { pending, limit } => {
                debug_assert!(pending, "clap enforces --pending (required = true)");
                cmd::extract::run(&dir, limit).await
            }
            AdminCmd::Index { documents, embed } => cmd::index::run(&dir, documents, embed).await,
            AdminCmd::Reextract { space } => cmd::reextract::run(&dir, &space).await,
            AdminCmd::Model { command } => cmd::model::run(&command).await,
            AdminCmd::Migrate { dry_run } => cmd::migrate::run(dry_run),
            AdminCmd::Eval { suite } => cmd::eval::run(&suite).await,
            AdminCmd::Space { command } => match command {
                cli::SpaceCmd::Add { name } => {
                    cmd::space_add::run(&dir, explicit_dir, home.as_deref(), &name).await
                }
                cli::SpaceCmd::Remove { name, purge } => {
                    cmd::space_remove::run(&dir, home.as_deref(), &name, purge).await
                }
            },
            AdminCmd::Skill { command } => match command {
                cli::SkillCmd::Install { target } => cmd::skill::run(&target).await,
            },
        },
    }
}
