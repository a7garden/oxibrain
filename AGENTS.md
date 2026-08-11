# AGENTS.md — oxibrain

> oxibrain is a standalone, local-first second brain: an immutable episode ledger
> plus a fully rebuildable knowledge projection, served to humans (CLI) and agents
> (MCP, Rust API). This file is the project conventions guide; the architecture
> lives in `doc/DESIGN.md`, which is authoritative. How the oxi apps compose around
> it — and the cross-project roadmap — lives in `doc/ECOSYSTEM.md`.

## Project Stack

oxibrain is greenfield. **It is a standalone product, not an oxios component** —
a default build must pull zero oxi-ecosystem crates.

- **Language / edition:** Rust 2024.
- **Runtime:** tokio. No Python, no external database process, no graph database.
- **Storage:** SQLite (`rusqlite`, bundled) + `sqlite-vec`; HNSW in memory.
  oxibrain owns its store and its migrations.
- **LLM:** behind `LlmPort` (`oxibrain-ports`). Adapters: HTTP providers
  (default), `oxicode-ai` (optional feature), fakes (tests). Never a direct
  dependency on `oxicode-sdk` or any `oxios-*` crate.
- **Embeddings:** behind `EmbeddingPort`. TF-IDF baseline (offline), GGUF dense
  (aarch64) as an adapter.
- **MCP:** `rmcp` (official Rust SDK) for protocol/transport; oxibrain owns tool
  semantics only. Decision gate at M3 (`doc/DESIGN.md` §12.2).
- **Workspace:** Cargo workspace, crates under `crates/` (`doc/DESIGN.md` §15).
- **Frontend (M5 only):** Vite + React 19 + Tailwind v4. Out of scope until M5.

**Shape of the product** (`doc/DESIGN.md` §1.3–1.4): one engine, three shapes —
the `oxibrain` crate (library), **one** `oxibrain` binary (CLI + MCP server +
daemon as subcommands), and an M5 desktop brain UI. v1 ships crate + binary; the
CLI must be a complete product with no GUI. oxibrain is **not a markdown editor**
and never owns authoring — vaults are read through connectors.

Package manager: `cargo`; `bun` for the M5 frontend.

## Commands

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo deny check

# The standalone guarantee — must pass, no oxi crates in the tree
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1   # expect no match

# Eval (M2+): `fast` replays cached LLM responses, no network
cargo run -p oxibrain-cli -- eval --suite fast
cargo run -p oxibrain-cli -- eval --suite full    # live provider, nightly only
```

## Code Style

- `clippy` clean with `-D warnings`; `#![cfg_attr(test, allow(clippy::unwrap_used))]`
  so production code is linted and `.unwrap()` is test-only.
- `expect("reason")` for invariants, `?` for fallible ops. No bare `unwrap` in
  non-test code.
- Public APIs return typed errors (`BrainError`), never `anyhow` across a crate
  boundary. `anyhow` is fine internally.
- Module-per-file; `lib.rs` is an index, logic lives in focused submodules.
- Comments, doc-comments, commit messages, and design docs in **English**.
  Korean is for chat, not source.
- Time is always explicit: `Timestamp` (UTC), never a bare `i64` in a signature.
  Clock access goes through `ClockPort` so tests can control time.

## Architecture invariants

**`doc/DESIGN.md` §3 is the contract. Violating one of these is a bug even if
tests pass; changing one requires revising the design doc first.**

- **P1 Ledger and projection.** Episodes are immutable and append-only.
  Everything else is derived and must be reconstructible by `reproject()`. If you
  add derived state that reprojection cannot rebuild, you have broken the system.
  Two corollaries with teeth:
  - **The ledger is the only durable write path.** Manual writes (`declare`,
    `merge`, `retract`) create `Declaration` episodes. Never write a projection
    row that no episode explains — reprojection would erase it.
  - **The projection is byte-identical across rebuilds.** IDs are content-derived
    (DESIGN §5.6), replay order is canonical. Never introduce a random ULID, a
    wall-clock value, or a map iteration order into anything persisted.
- **`EpisodeKind::Derived` is terminal.** Consolidation and community summaries
  write derived episodes; nothing is ever extracted from one, and their generated
  text is cached. Extracting from a derived episode creates a feedback loop that
  destroys determinism.
- **P2 Assertions, not facts.** Never store "X is true". Store "episode E, via
  extractor R, claimed X over interval I". Truth is folded from assertions.
- **P3 Stable identity, reversible resolution.** Entity IDs are derived from
  first-mention location, never from names — so a rename or merge never changes
  one. Every assertion keeps its verbatim `Mention`, which is what makes
  re-resolution exact rather than best-effort.
- **P4 Semantics in the registry.** Cardinality, temporality, invalidation, and
  types come from the predicate registry — never hard-coded in the pipeline and
  never only in a prompt. The extraction JSON Schema is *generated* from it.
- **P5 Forgetting is not deleting.** Decay and consolidation change salience and
  emit derived episodes. Only `redact()` destroys, and it is audited.
- **P6 Engine is a library.** `oxibrain-core` knows nothing of MCP/HTTP/CLI.
  Anything reachable over MCP is reachable in-process, and vice versa.
- **P7 Ports at the boundary.** LLM, embedding, and clock are oxibrain traits.
  No provider SDK leaks into core.
- **P8 One writer per store.** Enforced by advisory lock. Multi-app access goes
  through the daemon.

## Boundaries

- Do **not** add a dependency from `oxibrain-core` to any adapter crate, to
  `oxicode-*`, or to `oxios-*`. CI enforces this.
- Do **not** touch `rusqlite` outside `oxibrain-store`.
- Do **not** open a transaction across an LLM call, a network call, or an
  embedding computation.
- Do **not** write schema changes without a migration + an up-test from the
  previous version fixture.
- Do **not** hard-code resolution thresholds or predicate semantics. Config and
  registry, respectively.
- Do **not** modify `oxios` in M0–M4. The oxios migration is M5 and is
  decoupled by design. No re-export shims in either direction.
- Do **not** add authoring/editing features — no markdown editor, no vault
  management, no file writes into a user's notes. oxibrain reads sources through
  connectors. Quick capture (M5) writes episodes, never files.
- Do **not** let extraction output reach `beliefs` without passing the validator.
  Invalid output goes to `extraction_failures`, never silently dropped.

## Testing expectations

- Property tests for the temporal fold, interval algebra, and resolution decisions.
- The **reprojection equivalence test** (incremental projection == full reproject)
  is the highest-value test in the suite. It must never be disabled.
- Crash tests at each ingest stage boundary; assert resume with no duplicates.
- Migration chain test from every historical schema version.
- New extraction behavior needs golden-corpus coverage before it merges (M2+).

## Git

- Squash merge. Conventional commits: `feat:`, `fix:`, `chore:`, `docs:`,
  `refactor:`, `test:`, `perf:`.
- Branches: `type/short-description` (e.g. `feat/temporal-fold`,
  `fix/resolution-blocking`).
- Commit messages in English; body on a separate line if needed.
- Design changes land in `doc/DESIGN.md` with `docs:` and bump its version header.
- Non-obvious architectural choices get an ADR in `doc/adr/`.
