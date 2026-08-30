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
    /// Orientation: spaces with counts, document freshness, versions — the
    /// agent's first call and the `space` enum source (agent-first-cli §7).
    Describe,
    /// Registry introspection: per-op schema, caps, mutating (agent-first-cli §7).
    Schema {
        /// Op name; omit for the full catalogue.
        op: Option<String>,
    },
    /// Agent op dispatch: `oxibrain <op> --json PAYLOAD` — the ops, 1:1
    /// with MCP tools/list. The payload is the MCP `tools/call` arguments
    /// object; stdout carries one JSON envelope (agent-first-cli §2–§3).
    #[command(external_subcommand)]
    Op(Vec<String>),
    /// Machine/product lifecycle verbs — NOT the agent surface (spec §10).
    /// These never appear in `tools/list` and do not count against the cap.
    Admin {
        #[command(subcommand)]
        command: AdminCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum AdminCmd {
    /// Initialize a new brain store.
    Init {
        #[arg(long)]
        space: Option<String>,
    },
    /// Show store statistics.
    Stats,
    /// List all spaces with counts. Read-only.
    Spaces,
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
    /// Undo the most recent merge for an entity (D34) — console repair.
    EntitySplit {
        surface: String,
        ty: String,
        #[arg(long)]
        space: String,
    },
    /// Reproject the store.
    Reproject,
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
        /// Target space (required — no resolution chain, ADR-013).
        #[arg(long)]
        space: String,
    },
    /// Token management (DESIGN §12.4: `token issue|list|revoke`).
    Token {
        #[command(subcommand)]
        command: TokenCmd,
    },
    /// Serve the brain over stdio (default) or loopback HTTP.
    Serve {
        /// Serve a newline-delimited JSON-RPC session on stdin/stdout (the
        /// default when no transport flag is given). A caller-owned child —
        /// the daemonless transport of the two-plane design.
        #[arg(long, conflicts_with = "http")]
        stdio: bool,
        /// Serve loopback HTTP (e.g. `127.0.0.1:8080`) with the operations
        /// console. Foreground only.
        #[arg(long)]
        http: Option<String>,
        /// Serve the desktop brain UI from this directory (GET requests).
        /// Dev override — defaults to the embedded bundle (see ADR-008).
        #[arg(long)]
        ui_dir: Option<PathBuf>,
    },
    /// Predicate registry (DESIGN §12.4: `predicate add|list`).
    Predicate {
        #[command(subcommand)]
        command: PredicateCmd,
    },
    /// Drain uncached memory-plane episodes through the configured
    /// extractor (the operator repair and batch path, spec §9.5).
    Extract {
        /// Extract the uncached backlog (replaces `extract <episode-id>`).
        #[arg(long, required = true)]
        pending: bool,
        /// Stop after N episodes (default: drain the whole backlog).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Reconcile the documents cache from the configured roots (spec §7.3).
    Index {
        /// The spec's canonical spelling; documents indexing is the default
        /// behavior of this command with or without the flag.
        #[arg(long)]
        documents: bool,
        /// Also embed every missing document chunk (dense coverage).
        #[arg(long)]
        embed: bool,
    },
    /// Re-extract all primary episodes with the configured extractor.
    Reextract {
        #[arg(long)]
        space: String,
    },
    /// Migrate legacy Oxi-home layouts into the canonical tree (resumable).
    /// Copies `<oxi-home>/models` into `<oxi-home>/brain/models`; the source
    /// is kept as a recoverable backup and the run is journaled, so it is
    /// safe to re-run after a crash or interruption.
    Migrate {
        /// Preview source, destination, size, and conflicts — mutates nothing.
        #[arg(long)]
        dry_run: bool,
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
    /// Source management.
    Source {
        #[command(subcommand)]
        command: SourceCmd,
    },
    /// Manage spaces (spec: space lifecycle).
    Space {
        #[command(subcommand)]
        command: SpaceCmd,
    },
    /// Generate/install the agent skill (SKILL.md + CONTEXT.md) from the op
    /// registry (spec §10: `admin skill install`; derived, never
    /// hand-maintained).
    Skill {
        #[command(subcommand)]
        command: SkillCmd,
    },
}

// ── Nested subcommand groups (DESIGN §12.4) ────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum SkillCmd {
    /// Install to a target: `omp` (~/.omp/skills/oxibrain), `claude`
    /// (~/.claude/skills/oxibrain), or `raw` (print to stdout).
    Install {
        #[arg(long, default_value = "omp")]
        target: String,
    },
}

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
        #[arg(long)]
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
    /// Register a custom predicate from JSON.
    Add {
        /// Full PredicateDef JSON.
        json: String,
        #[arg(long)]
        space: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SourceCmd {
    /// Set trust policy for a source.
    Policy {
        /// Source name (as registered).
        name: String,
        /// Trust tier: trusted | untrusted.
        #[arg(long)]
        trust: String,
        /// Effective from (epoch ms). Defaults to now.
        #[arg(long)]
        effective_from: Option<i64>,
        /// Effective to (epoch ms). Open-ended if omitted.
        #[arg(long)]
        effective_to: Option<i64>,
        #[arg(long)]
        space: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SpaceCmd {
    /// Create a space (idempotent) and provision its vault dir + root.
    ///
    /// Names that begin with `-` are not directly accepted by clap; pass the
    /// name after `--` (e.g. `oxibrain space add -- -draft`) which clap
    /// forwards verbatim.
    Add {
        /// Space name. See `validate_space_name` (Task 1) for rules.
        name: String,
    },
    /// Remove a space. Refuses by default if the space has episodes,
    /// document chunks, or documents.toml roots referencing it; pass
    /// `--purge` to drop everything (audited redaction).
    Remove {
        /// Space name.
        name: String,
        /// Drop all data and clear the documents cache (audited).
        #[arg(long)]
        purge: bool,
    },
}
