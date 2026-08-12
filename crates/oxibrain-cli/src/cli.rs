use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "oxibrain",
    version,
    about = "A second brain for humans and agents"
)]
pub struct Cli {
    #[arg(long, env = "OXIBRAIN_DIR", global = true)]
    pub dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a new brain store.
    Init {
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Ingest a file or stdin as an episode.
    Ingest {
        /// File path, or `-` for stdin.
        path: PathBuf,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Show store statistics.
    Stats,
    /// Health check.
    Doctor,
    /// Back up the store.
    Backup {
        #[arg(long)]
        no_projection: bool,
        #[arg(long)]
        no_cache: bool,
        /// Output directory (default: sibling of store dir).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Restore from a backup.
    Restore {
        backup: PathBuf,
    },
    /// Ask a question (hybrid query).
    Ask {
        question: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Show entity beliefs.
    EntityShow {
        id: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Timeline for an entity.
    Timeline {
        entity_id: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Provenance for a statement.
    Why {
        statement_id: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// List contradicted statements.
    Contradictions {
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Reproject the store.
    Reproject,
    /// Redact (the only true delete).
    Redact {
        target: String,
        #[arg(long, default_value = "personal")]
        space: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        reason: String,
    },
    /// Export to JSONL.
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import from JSONL.
    Import {
        file: PathBuf,
    },
    /// Token management.
    TokenIssue {
        #[arg(long, default_value = "personal")]
        space: String,
        #[arg(long)]
        caps: String,
        #[arg(long)]
        label: Option<String>,
    },
    TokenList,
    TokenRevoke {
        id: String,
    },
    Serve {
        /// Listen on a Unix-domain socket path instead of stdio.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Listen on loopback HTTP (e.g. `127.0.0.1:8080`) instead of stdio.
        #[arg(long)]
        http: Option<String>,
        /// Require token authentication on socket connections (DESIGN §11.2).
        #[arg(long)]
        require_token: bool,
    },
    /// List predicates in the core/v1 registry.
    PredicateList,
    /// Extract a single episode (calls the LLM, validates, projects).
    Extract {
        /// Episode ID to extract.
        episode_id: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Re-extract all primary episodes with the configured extractor.
    Reextract {
        #[arg(long, default_value = "personal")]
        space: String,
    },

    /// Run the extraction evaluation suite (DESIGN §14.2).
    Eval {
        /// Suite: `fast` (fixture-replayed, no network) or `full` (live provider).
        #[arg(long, default_value = "fast")]
        suite: String,
    },
}
