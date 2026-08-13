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
    Restore { backup: PathBuf },
    /// Ask a question (hybrid query).
    Ask {
        question: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Entity management (DESIGN §12.4: `entity show|merge|split|alias`).
    Entity {
        #[command(subcommand)]
        command: EntityCmd,
    },
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
    Import { file: PathBuf },
    /// Import from an oxios-memory SQLite database (DESIGN §16.3).
    ImportOxios {
        /// Path to the oxios-memory `memory.db` file.
        db: PathBuf,
        /// Target space (default: personal).
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Token management (DESIGN §12.4: `token issue|list|revoke`).
    Token {
        #[command(subcommand)]
        command: TokenCmd,
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
        /// Run as a background daemon: write a PID file and shut down
        /// gracefully on SIGTERM/SIGINT (DESIGN §4.3, §15). External
        /// supervision (launchd) handles backgrounding; this flag does not fork.
        #[arg(long)]
        daemon: bool,
        /// Serve the desktop brain UI from this directory (GET requests).
        /// Point at the built `apps/brain-ui/dist/`.
        #[arg(long)]
        ui_dir: Option<PathBuf>,
    },
    /// Predicate registry (DESIGN §12.4: `predicate add|list`).
    Predicate {
        #[command(subcommand)]
        command: PredicateCmd,
    },
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
    /// Model artifact management (§8.4: `model list|pull|verify|use`).
    Model {
        #[command(subcommand)]
        command: ModelCmd,
    },

    /// Run the extraction evaluation suite (DESIGN §14.2).
    Eval {
        /// Suite: `fast` (fixture-replayed, no network) or `full` (live provider).
        #[arg(long, default_value = "fast")]
        suite: String,
    },
}

// ── Nested subcommand groups (DESIGN §12.4) ────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum ModelCmd {
    /// List installed models and their verification status.
    List,
    /// Download the default model set (or a named model).
    Pull {
        /// Model name or file. Omit to pull the whole default set.
        name: Option<String>,
    },
    /// Re-hash installed models against the manifest.
    Verify {
        /// Model name. Omit to verify all.
        name: Option<String>,
    },
    /// Resolve the active model for extraction (prints path + digest).
    Use { name: String },
}

#[derive(Subcommand, Debug)]
pub enum TokenCmd {
    /// Issue a new token (returns the secret once).
    Issue {
        #[arg(long, default_value = "personal")]
        space: String,
        #[arg(long, help = "Comma-separated capabilities (Read,Ingest,Write,Sample)")]
        caps: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// List all tokens (secrets redacted).
    List,
    /// Revoke a token by id.
    Revoke { id: String },
}

#[derive(Subcommand, Debug)]
pub enum PredicateCmd {
    /// List predicates in the core/v1 registry.
    List,
}

#[derive(Subcommand, Debug)]
pub enum EntityCmd {
    /// Show entity beliefs.
    Show {
        id: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
}
