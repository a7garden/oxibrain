# oxibrain

A standalone, local-first second brain for humans and agents: an immutable
episode ledger plus a deterministic semantic truth projection. Derived search
indexes and ranking metadata can be rebuilt separately.

## Highlights

- **No account, no API key, no external services.** Extraction and embeddings
  run on local GGUF models by default; HTTP providers are an optional quality
  tier. A default build pulls zero oxi-ecosystem crates.
- **One engine, three shapes** — a Rust library (`oxibrain`), one binary
  (`oxibrain`: CLI + caller-owned server), and a desktop brain UI.
- **Agent-native.** Sixteen MCP tools (capped) over caller-owned stdio or a
  foreground loopback HTTP session. Anything reachable over MCP is reachable
  in-process, and vice versa.
- **Assertions, not facts.** The ledger records who claimed what, over which
  interval; knowledge is folded from assertions, and every derived summary
  carries its sources and uncertainty.
- **No language is privileged.** Character n-grams and multilingual embeddings;
  no stemmers, no stopword lists, no script checks. Retrieval and resolution
  quality is held to parity across writing systems.

## Table of contents

- [Install](#install)
- [Quick start](#quick-start)
- [Source retrieval](#source-retrieval)
- [Backup and restore](#backup-and-restore)
- [Agent skill](#agent-skill)
- [Architecture](#architecture)
- [Project structure](#project-structure)
- [Oxi ecosystem](#oxi-ecosystem)
- [Documentation](#documentation)
- [Development](#development)
- [Contributing](#contributing)
- [License](#license)

## Install

```bash
cargo install oxibrain-cli
```

The binary is named `oxibrain`.

Apple silicon macOS builds are also attached to each
[GitHub release](https://github.com/a7garden/oxibrain/releases). Download
the four assets from the exact version tag and verify the `.sha256` files
before extracting; the standard archive contains `oxibrain`, and the MLX
archive contains `oxibrain-mlx`.

### Managed install (ecosystem standard)

Hosts that supervise the binary (oxios `BrainInstaller`, or
`oxios brain install`) place it at the ecosystem-standard location:

```text
~/.oxi/oxibrain/bin/oxibrain        # launcher symlink → ../versions/<v>/oxibrain
~/.oxi/oxibrain/versions/<v>/       # one directory per release (newest 2 kept)
```

`cargo install oxibrain-cli` (→ `~/.cargo/bin/oxibrain`) stays a fully
supported channel; managed and cargo installs coexist, with the managed
launcher taking precedence in hosts that resolve both.

## Quick start

```bash
# Create the store and a brain space (more spaces: oxibrain admin space add <name>).
oxibrain admin init --space personal

# Pull the local GGUF model set (otherwise pulled lazily on first extraction).
oxibrain admin model pull

# Register a document root by editing ~/.oxi/brain/documents.toml (there is no
# CLI verb for registration), then reconcile the documents cache and embed
# missing chunks.
oxibrain admin index --embed

# Drain uncached memory-plane episodes through the configured extractor.
oxibrain admin extract --pending

# Query the op surface: every op takes its JSON payload via --json or stdin.
oxibrain search --json '{"space":"personal","query":"database decision","planes":["documents"],"limit":5}'
echo '{"space":"personal","query":"database decision","limit":5}' | oxibrain search

# Serve caller-owned JSON-RPC over stdio for an MCP client integration.
oxibrain admin serve --stdio
```

The store lives under `~/.oxi`; every subcommand accepts `--dir <DIR>` (env
`OXIBRAIN_DIR`) to relocate it. `admin space add` provisions each space's
vault directory and document root. `oxibrain describe` and `oxibrain schema`
are the orientation and introspection entries into the op surface.

Agent `session_record` stores an **Untrusted Primary** with `SourceRef::Session`
provenance, not an owner declaration. Its event identity is the space, sessions
source and session ID: identical bytes replay to the same episode; different bytes
conflict without replacing it. `session_list` can exclude the caller's current
session. The live `session_record` input schema advertises
`"x-oxibrain-session-trust": "untrusted"`; callers requiring that trust boundary must
deny writes when the marker is absent, never substitute Note ingestion.

Agents use CLI operations directly or launch a caller-owned `admin serve --stdio`
child through `oxibrain-client`. The foreground `admin serve --http <address>`
variant exposes the local operations console.

## Source retrieval

`recall` assembles source passages with their conditions, competing claims and
provenance. It does not generate an authoritative answer. Inspect operation
receipts under the CLI envelope's `data.meta`; `partial`, channel caps, pending
indexing and skipped roots describe limits on the evidence considered. A hit
does not prove an answer, and an empty read does not prove absence.

Raw memory windows can be indexed without extraction. With the configured local
embedding model available, explicitly build the optional semantic indexes:

```bash
oxibrain admin index --memory --embed
oxibrain admin index --documents --embed
oxibrain recall --json '{"space":"personal","query":"recovery conditions","token_budget":3000}'
```

Set `OXIBRAIN_TOKENIZER_JSON` to a local model tokenizer JSON for exact budgets.
Optional `OXIBRAIN_READ_POLICY_JSON` points to a policy JSON; for example,
`{"compact_evidence":true}` enables response-scoped E1/E2 citation labels.
The complete source map remains in `data.data.evidence`: preserve its reference,
revision, view and byte range. Rendered-context offsets are separate from source
offsets. Both rendering modes retain explicit omissions and count their actual
context with the configured tokenizer. Existing full rendering stays the default.

Memory stores upgrade to schema 17 in version 0.21.0. Keep a verified schema-15
backup before opening an existing store; older binaries cannot reopen it after
upgrade. Run `oxibrain admin index --memory` after upgrading to rebuild the
discarded source-search cache from the ledger; add `--embed` to rebuild optional
dense vectors. Reads report pending coverage until indexing completes. Source
documents remain read-only and are never converted.

## Shared local HTTP inference

Set `OXIBRAIN_LLM_PROVIDER=loopback`, `OXIBRAIN_LLM_BASE_URL=http://127.0.0.1:8768/v1`,
and `OXIBRAIN_LLM_MODEL=mlx-community/Qwen3.6-35B-A3B-4bit` when using the
Dyno-owned MLX endpoint. These are process-scoped overrides, not a change to the
default local engine. The HTTP adapter sends strict `response_format: json_schema`.
Extraction object variants use `anyOf` with a required `kind` and mutually
exclusive `entity`/`literal` constants: the accepted language is identical to
`oneOf`, without decoder coercion. Quote/span and predicate/type validation still
gate projection.

For a bounded integration check, use a new `OXIBRAIN_DIR`, ingest synthetic
content, and run `oxibrain admin extract --pending --limit 1`. Verify an accepted
extraction and its source-backed assertions; exit zero or “processed” alone does
not prove acceptance. Do not drain an existing backlog as a smoke test.

## Backup and restore

```bash
oxibrain --dir /path/to/brain admin backup --out /path/to/backup-generation
oxibrain --dir /path/to/empty-restore-target admin restore /path/to/backup-generation
```

Backup uses SQLite's online API so a WAL-backed brain can be snapshotted while
another process writes. The backup directory contains a generation manifest
with file digests and schema versions. Restore rejects a missing, changed, or
corrupt artifact and refuses to overwrite an existing directory. Use a new
directory for an isolated restore check; moving it into service is a separate
operator step.

The artifact includes brain.db, documents.toml when present, and model weights
under the brain directory's `models/` subdirectory. It excludes the rebuildable documents.db and all
external document roots and shared vaults; those source documents need their
own backup. `--no-projection` omits rebuildable memory projection rows.
`--no-cache` also omits extraction cache and brain-local model weights;
restore reprojects the ledger, and extracted knowledge needs
`admin extract --pending` afterward. Model weights configured outside the
brain directory are never copied. A local backup directory is still on the
same failure domain until copied to a separate, appropriately protected
destination and restored there.

`admin reproject` replays declarations and cached extractions together at their
recorded event times, preserving entity identities, assertion provenance,
denials, merges and retractions. It uses the extraction cache, not new model
requests. The ledger remains unchanged. Search indexes, salience and communities
are rebuilt: scores and ordering can differ from a stale incremental index,
while semantic truth stays stable; bounded search results may rerank. Repeated
reprojection is a fixed point of the rebuilt projection. Use an isolated restored
copy to verify this boundary before any operational reprojection.

## Agent skill

oxibrain ships its own agent skill — a `SKILL.md` + `CONTEXT.md` pair that
teaches an agent the op surface (all sixteen ops), the payload contract
(`space` is required on every op; ids are resolved, never guessed), and the
safety rails (dry-run plan tokens, `meta.dropped`, `locked` backoff). Install
it into your agent's skill directory:

```bash
oxibrain admin skill install                 # omp    → ~/.omp/skills/oxibrain (default)
oxibrain admin skill install --target claude # claude → ~/.claude/skills/oxibrain
oxibrain admin skill install --target codex  # codex  → $CODEX_HOME/skills/oxibrain
                                              #          or ~/.codex/skills/oxibrain
oxibrain admin skill install --target raw    # print both files to stdout
```

The `codex` target uses `$CODEX_HOME` when set and otherwise installs under
`~/.codex/skills/oxibrain`.

Both files are generated from the op registry (`oxibrain-ops`), not
hand-maintained, so they cannot drift from the installed binary's surface.
Re-run the install after upgrading so the skill matches your binary.

## Architecture

The immutable episode ledger is the durable source of truth. The knowledge
projection — entities, assertions, search indexes, vectors, and rendered views
— is derived from that ledger and can be rebuilt. The core engine stays free of
transport and provider dependencies; CLI, MCP, and desktop surfaces compose it
at the boundary. [ARCHITECTURE.md](doc/ARCHITECTURE.md) is authoritative.

## Project structure

| Crate | Role |
|---|---|
| `oxibrain-ports` | Port traits — LLM, embedding, tokenizer, rerank, clock |
| `oxibrain-core` | Domain types, temporal fold, extraction, ranking, packing |
| `oxibrain-index` | Lexical/graph primitives — n-gram, MinHash, adjacency |
| `oxibrain-store` | SQLite ledger, projection, migrations, queries |
| `oxibrain-views` | Pure renderers — Markdown briefs, exports |
| `oxibrain` | Facade library — the engine |
| `oxibrain-llm-local` | GGUF inference, grammar-constrained decoding |
| `oxibrain-llama-runtime` | Shared llama.cpp runtime initialization for local adapters |
| `oxibrain-llm-http` | HTTP LLM adapter (OpenAI-compatible) |
| `oxibrain-embed-local` | Multilingual embedding adapter |
| `oxibrain-connectors` | Source connectors — vault readers, file ingest |
| `oxibrain-client` | Client SDK — caller-owned stdio session |
| `oxibrain-mcp` | MCP server tools — sixteen-tool cap |
| `oxibrain-cli` | The `oxibrain` binary: CLI + caller-owned server |

## Oxi ecosystem

oxibrain is intentionally standalone: it owns its ledger, projection, and local
storage. [oxicode](https://github.com/project-oxi/oxicode),
[oxios](https://github.com/project-oxi/oxios), and
[oximemo](https://github.com/project-oxi/oximemo) use its public CLI or client
contracts for durable memory rather than depending on its storage internals. See
[the ecosystem guide](doc/ECOSYSTEM.md) for the boundary map.

## Documentation

- [`doc/ARCHITECTURE.md`](doc/ARCHITECTURE.md) — authoritative architecture
  and invariants (P1–P11)
- [`doc/ROADMAP.md`](doc/ROADMAP.md) — sequencing and milestone exit criteria
- [`doc/ECOSYSTEM.md`](doc/ECOSYSTEM.md) — how oxibrain composes with the oxi
  ecosystem
- [`doc/adr/`](doc/adr/) — architecture decision records

## Development

```bash
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test

# The standalone guarantee — no oxi crates in the tree
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1
```

Releases: pushing a `v*` tag (or manually dispatching the workflow with an
existing tag) publishes the matching crate versions, checks the exact published
version with a fresh consumer, and builds the macOS binaries. GitHub keeps the
release as a draft until both archives and checksums have been downloaded back
and verified, and both binaries report the tagged version and contain only
ARM64 code (`.github/workflows/publish.yml`,
`scripts/verify-release-assets.sh`).

## Contributing

Read [AGENTS.md](AGENTS.md) before contributing. It defines the architectural
invariants, quality gates, and documentation authority for this repository.

## License

MIT OR Apache-2.0
