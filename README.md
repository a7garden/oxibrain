# oxibrain

A standalone, local-first second brain for humans and agents: an immutable
episode ledger plus a knowledge projection that can be rebuilt from it,
byte for byte.

- **No account, no API key, no external services.** Extraction and embeddings
  run on local GGUF models by default; HTTP providers are an optional quality
  tier. A default build pulls zero oxi-ecosystem crates.
- **One engine, three shapes** — a Rust library (`oxibrain`), one binary
  (`oxibrain`: CLI + MCP server + daemon), and a desktop brain UI.
- **Agent-native.** Fifteen MCP tools (capped) over stdio or a Unix-domain
  socket. Anything reachable over MCP is reachable in-process, and vice versa.
- **Assertions, not facts.** The ledger records who claimed what, over which
  interval; knowledge is folded from assertions, and every derived summary
  carries its sources and uncertainty.
- **No language is privileged.** Character n-grams and multilingual embeddings;
  no stemmers, no stopword lists, no script checks. Retrieval and resolution
  quality is held to parity across writing systems.

## Install

```bash
cargo install oxibrain-cli
```

The binary is named `oxibrain`. macOS and Linux (Unix-domain sockets).

## Quick start

```bash
oxibrain init                        # create the store — instant, offline
oxibrain model pull                  # fetch local models (resumable, digest-verified)
oxibrain sync ~/notes                # ingest a directory of markdown notes
oxibrain ask "who works at Acme?"    # hybrid query (lexical + vector + graph)
oxibrain page <entity-id>            # brief with followable links
oxibrain serve                       # MCP server on stdio (Claude Desktop etc.)
oxibrain serve --daemon              # daemon on ~/.oxi/brain/oxibrain.sock
```

Agents connect through MCP (`serve`) or the client SDK (`oxibrain-client`,
typed handshake over `~/.oxi/brain/oxibrain.sock`, override with
`$OXIBRAIN_SOCKET`). Scoped tokens: `oxibrain token issue --caps Read`.

Local model defaults: Qwen2.5-1.5B-Instruct (grammar-constrained extraction)
and BGE-M3 (multilingual embeddings). `OXIBRAIN_MODELS_DIR` points at a
pre-pulled directory for air-gapped installs. Oxi Foundation profiles and
OpenAI/Anthropic-compatible env keys select stronger extractors when present.

## Workspace

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
| `oxibrain-client` | Client SDK — typed handshake over the socket |
| `oxibrain-mcp` | MCP server tools — fifteen-tool cap |
| `oxibrain-cli` | The `oxibrain` binary: CLI + MCP server + daemon |

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
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

# The standalone guarantee — no oxi crates in the tree
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1
```

Releases: tag `v*` publishes all crates in dependency order and creates the
GitHub release (`.github/workflows/publish.yml`, `scripts/publish.sh`).

## License

MIT OR Apache-2.0
