# AGENTS.md — oxibrain

> oxibrain is a standalone, local-first second brain: an immutable episode ledger
> plus a fully rebuildable knowledge projection, served to humans (CLI) and agents
> (MCP, Rust API). This file is the project conventions guide; the architecture
> lives in `doc/ARCHITECTURE.md`, which is authoritative. Sequencing lives in
> `doc/ROADMAP.md`; per-milestone implementation specs in `doc/spec/`; how the oxi
> apps compose in `doc/ECOSYSTEM.md`.
>
> `doc/DESIGN.md` was renamed to `doc/ARCHITECTURE.md` at v2.0 — "DESIGN.md" now
> commonly denotes a front-end design-system document. Older doc-comments and
> archived specs may still say `DESIGN.md §n`; the section numbers changed in v2.0,
> so treat those references as historical.

## Project Stack

oxibrain is greenfield. **It is a standalone product, not an oxios component** —
a default build must pull zero oxi-ecosystem crates, and no required external
service, account, or API key.

- **Language / edition:** Rust 2024.
- **Runtime:** tokio. No Python, no external database process, no graph database.
- **Storage:** SQLite (`rusqlite`, bundled) + `sqlite-vec`.
  oxibrain owns its store and its migrations.
- **Inference:** behind `LlmPort` (`oxibrain-ports`). **Default is local** —
  `oxibrain-llm-local` (GGUF, grammar-constrained decoding). HTTP providers and
  MCP client sampling are optional quality tiers. Never a direct dependency on
  `oxicode-sdk` or any `oxios-*` crate. (`ARCHITECTURE.md` §8)
- **Embeddings:** behind `EmbeddingPort`. **Default is local and multilingual**
  (`oxibrain-embed-local`). Character-n-gram vectors are the pre-model fallback,
  and are not called "semantic".
- **Tokenization:** behind `TokenizerPort`, supplied by the model. Token budgets
  are counted, never estimated (`ARCHITECTURE.md` §7.5).
- **MCP:** oxibrain owns tool semantics; **fifteen tools is the cap**
  (`ARCHITECTURE.md` §16.2).
- **Workspace:** Cargo workspace, crates under `crates/` (`ARCHITECTURE.md` §18).
- **Frontend:** Vite + React 19 + Tailwind v4, in `apps/`.

**Shape of the product** (`ARCHITECTURE.md` §1.3–1.4): one engine, three shapes —
the `oxibrain` crate (library), **one** `oxibrain` binary (CLI + MCP server +
daemon as subcommands), and a desktop brain UI. The CLI must be a complete product
with no GUI. oxibrain is **not a markdown editor** and never owns authoring —
vaults are read through connectors.

Package manager: `cargo`; `bun` for the frontend.

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

**`doc/ARCHITECTURE.md` §3 is the contract. Violating one of these is a bug even
if tests pass; changing one requires revising the architecture doc first.**

- **P1 Ledger and projection.** Episodes are immutable and append-only.
  Everything else is derived and must be reconstructible by `reproject()`. If you
  add derived state that reprojection cannot rebuild, you have broken the system.
  Three corollaries with teeth:
  - **The ledger is the only durable write path.** Manual writes (`declare`,
    `merge`, `retract`) create `Declaration` episodes. Never write a projection
    row that no episode explains — reprojection would erase it.
  - **The truth half is byte-identical across rebuilds** — entities, keys, merges,
    statements, assertions, mentions, beliefs, predicates. IDs are deterministically
    derived (§5.6), replay order is canonical. Never introduce a random ULID, a
    wall-clock value, or a map iteration order into anything persisted here.
  - **The ranking half is equivalent, not identical** — vectors, FTS, chunks,
    adjacency, communities, salience. It may contain float embeddings. **Nothing
    in the ranking half may ever be read by the fold.**
- **Event identity is source occurrence, not byte equality.** For new-path
  episodes, identity is `(space_id, source_id, occurrence_id)`; `content_hash`
  verifies bytes and must never be used to collapse equal content from independent
  sources. Schema v10 intentionally has no `UNIQUE(space_id, content_hash)`.
- **Trust is server-evaluated ledger state.** Clients submit provenance claims,
  never an authoritative `TrustTier`; effective trust comes from source-policy
  `Declaration` episodes. `TrustedIngest` is the sole capability that may bypass
  policy and explicitly mark an ingest as trusted.
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
  never only in a prompt. The extraction JSON Schema **and the GBNF grammar** are
  *generated* from it.
- **P5 Forgetting is not deleting.** Decay and consolidation change salience and
  emit derived episodes. Only `redact()` destroys, and it is audited.
- **P6 Engine is a library.** `oxibrain-core` knows nothing of MCP/HTTP/CLI.
  Anything reachable over MCP is reachable in-process, and vice versa.
- **P7 Ports at the boundary.** Inference, embedding, tokenization, reranking and
  clock are oxibrain traits. No provider SDK leaks into core.
- **P8 One writer per store.** Enforced by advisory lock. Multi-app access goes
  through the daemon.
- **P9 Decision and data are separate.** *Store fetches and writes. Core decides.
  Facade sequences.* A function that both reads the database and chooses an
  outcome is a design error even when correct — it cannot be property-tested, and
  its filters get dropped silently. `fold` is the archetype; make new code look
  like it.
- **P10 Compression may lose detail, never doubt.** Every derived or summarized
  artifact carries the uncertainty computed from its support (contradictions,
  single-source claims, staleness, trust exclusions). A summary is never returned
  without its sources.
- **P11 No language is privileged.** No stemmer, no stopword list, no script
  branch, no language detector. Lexical matching is character-n-gram based;
  semantic matching uses multilingual embeddings; token counts come from the
  model's tokenizer. Language-specific knowledge is registry **data**, defaults to
  empty, and is never required for correctness.

## Boundaries

- Do **not** add a dependency from `oxibrain-core` to any adapter crate, to
  `oxicode-*`, or to `oxios-*`. CI enforces this.
- Do **not** name `rusqlite`, `tokio`, or `reqwest` in `oxibrain-core` or
  `oxibrain-index`. Do **not** name `rusqlite` in `oxibrain-views`.
- Do **not** name `rank`, `pack`, or `step` in `oxibrain-store`. It may name their
  input and output types. This is the enforceable form of **P9**.
- Do **not** put a word list, stemmer, or script check in any crate other than
  `oxibrain-index` — and it should not need one. Registry affix lists are data and
  are exempt. This is the enforceable form of **P11**.
- Do **not** open a transaction across an inference call, a network call, or an
  embedding computation.
- Do **not** write schema changes without a migration + an up-test from the
  previous version fixture.
- Do **not** hard-code resolution thresholds or predicate semantics. Config and
  registry, respectively.
- Do **not** add authoring/editing features — no markdown editor, no vault
  management, no file writes into a user's notes. oxibrain reads sources through
  connectors. Quick capture writes episodes, never files.
- Do **not** let extraction output reach `beliefs` without passing the validator.
  Invalid output goes to `extraction_failures`, never silently dropped.
- Do **not** exceed **fifteen** MCP tools. Adding one requires removing one.
- Do **not** accept a filter in a type and ignore it in the executor. That
  happened three times (F1, F3, F11) and is why P9 exists.

## Testing expectations

- Property tests for the temporal fold, interval algebra, resolution decisions,
  n-gram similarity, and every pure decision function (`rank`, `pack`, `step`).
- The **truth reprojection determinism test** (incremental projection == full
  reproject, byte-identical) is the highest-value test in the suite. It must never
  be disabled. Its ranking-half sibling asserts equivalence, not equality.
- **Conservation:** `rank` must place every candidate in exactly one of `items` or
  `dropped`. This is what makes "instrument what you discard" structural.
- Crash tests at each ingest stage boundary; assert resume with no duplicates.
- Migration chain test from every historical schema version.
- New extraction behavior needs golden-corpus coverage before it merges.
- **Parity suite:** no retrieval or resolution metric may vary more than 10
  percentage points across writing-system property classes (`ARCHITECTURE.md`
  §7.8). This is the executable form of P11.

## Git

- Squash merge. Conventional commits: `feat:`, `fix:`, `chore:`, `docs:`,
  `refactor:`, `test:`, `perf:`.
- Branches: `type/short-description` (e.g. `feat/temporal-fold`,
  `fix/resolution-blocking`).
- Commit messages in English; body on a separate line if needed.
- Architecture changes land in `doc/ARCHITECTURE.md` with `docs:` and bump its
  version header. Sequencing changes land in `doc/ROADMAP.md`.
- Non-obvious architectural choices get an ADR in `doc/adr/`.
