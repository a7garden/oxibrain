# AGENTS.md — oxibrain

> oxibrain is the unified memory & knowledge platform for the oxi ecosystem
> (Knowledge Graph × MCP × Agent). This file is the project conventions guide;
> the architecture lives in `doc/DESIGN.md`.

## Project Stack

oxibrain is greenfield. The stack below is the **intended** baseline; it tracks
the oxi ecosystem conventions and the existing substrate it builds on.

- **Language / edition:** Rust 2024 (matches `oxios`).
- **Runtime:** tokio (async). No Python, no external database process.
- **Engine:** `oxicode-sdk` for LLM calls (extraction pipeline).
- **Storage:** SQLite (`rusqlite` bundled) + `sqlite-vec` for vectors; HNSW in-memory.
- **Embeddings:** TF-IDF (lexical) + GGUF dense (aarch64) — inherited from the substrate.
- **MCP:** native MCP server adapter (`oxibrain-mcp`).
- **Workspace:** Cargo workspace, crates under `crates/` (see `doc/DESIGN.md` §9).
- **Frontend (Phase 4 only):** Vite + React 19 + Tailwind v4, per the oxi `DESIGN.md`.
  Out of scope until Phase 4.

Package manager: `cargo` (Rust); `bun` (frontend, Phase 4).

## Commands

```bash
# Build / test / lint (Rust)
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

# Feature flags (mirror the substrate)
cargo test -p oxibrain-core --features "sqlite-memory"
cargo test -p oxibrain-core --features "embedding-gguf"   # aarch64 only

# MCP server (Phase 2)
cargo run -p oxibrain-mcp
```

Exact feature names are finalized in the Phase-1 spec; keep the substrate's
`sqlite-memory` / `embedding-gguf` naming.

## Code Style

Follow `oxios` conventions (the parent ecosystem):

- `clippy` clean with `-D warnings`; `#![cfg_attr(test, allow(clippy::unwrap_used))]`
  so production code is linted and `.unwrap()` is test-only.
- Prefer `expect("reason")` for invariants, `?` for fallible ops. No bare `unwrap`
  in non-test code.
- Module-per-file, re-export the public surface from `lib.rs`. Keep `lib.rs` as an
  index; put logic in focused submodules.
- Comments and doc-comments in **English**. Korean is for chat, not source.

## Architecture

**`doc/DESIGN.md` is authoritative.** Summary:

- `oxibrain-core` — KG + memory engine: entity/relation/observation/fact store,
  LLM extraction pipeline, hybrid query (BM25 + vector + Think-on-Graph, RRF).
- `oxibrain-mcp` — MCP server adapter (tools/resources) over core.
- `oxibrain-memory` / `oxibrain-markdown` / `oxibrain-transport` — promoted from
  the oxios leaf crates (Phase 3). During Phase 1, depend on the *published*
  `oxios-memory` crate instead.

Key invariants (do not violate without updating DESIGN.md):
- **Provenance is mandatory** — every entity/relation/observation links to its source episode.
- **Bi-temporal edges** — relations are versioned (valid time + transaction time), never overwritten.
- **The substrate owns embeddings** — the KG layer never reimplements vector math.

## Boundaries

- Do **not** modify `oxios` crate internals during Phase 1 — consume the published
  `oxios-memory` crate. Physical promotion is a Phase-3 task.
- Do **not** introduce a graph database (Neo4j/FalkorDB) or a Python runtime. Embedded Rust only.
- Do **not** reimplement what the substrate provides (embeddings, HNSW, BM25, RRF, decay, dream).
- The extraction LLM prompt schema must stay in sync with the data model in `doc/DESIGN.md` §5.
- Migration shims (`oxios-*` re-exports) are temporary; track and retire them per Phase 3.

## Git

- Squash merge. Conventional commits: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`.
- Branches: `type/short-description` (e.g. `feat/kg-store`, `fix/extraction-resolve`).
- Commit messages in English; body on a separate line if needed.
- Keep the working tree clean; commit design changes to `doc/DESIGN.md` with `docs:`.
