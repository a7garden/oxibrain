# oxibrain

A standalone, local-first second brain for humans and agents: an immutable
episode ledger plus a knowledge projection that can be rebuilt from it,
byte for byte.

## Highlights

- **No account, no API key, no external services.** Extraction and embeddings
  run on local GGUF models by default; HTTP providers are an optional quality
  tier. A default build pulls zero oxi-ecosystem crates.
- **One engine, three shapes** — a Rust library (`oxibrain`), one binary
  (`oxibrain`: CLI + caller-owned server), and a desktop brain UI.
- **Agent-native.** Fifteen MCP tools (capped) over caller-owned stdio or a
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
# Create a local brain space and document root.
oxibrain admin init --space personal
oxibrain admin space add dev

# Reconcile configured document roots.
oxibrain admin index --documents

# Search a space from the CLI.
oxibrain search --json '{"space":"personal","query":"database decision","planes":["documents"],"limit":5}'

# Serve caller-owned JSON-RPC over stdio for an agent integration.
oxibrain admin serve --stdio
```

Agents use CLI operations directly or launch a caller-owned `admin serve --stdio`
child through `oxibrain-client`. The foreground `admin serve --http <address>`
variant exposes the local operations console.

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
| `oxibrain-llm-http` | HTTP LLM adapter (OpenAI-compatible) |
| `oxibrain-embed-local` | Multilingual embedding adapter |
| `oxibrain-connectors` | Source connectors — vault readers, file ingest |
| `oxibrain-client` | Client SDK — caller-owned stdio session |
| `oxibrain-mcp` | MCP server tools — fifteen-tool cap |
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

Releases: tag `v*` publishes all crates in dependency order and creates the
GitHub release (`.github/workflows/publish.yml`, `scripts/publish.sh`).

## Contributing

Read [AGENTS.md](AGENTS.md) before contributing. It defines the architectural
invariants, quality gates, and documentation authority for this repository.

## License

MIT OR Apache-2.0
