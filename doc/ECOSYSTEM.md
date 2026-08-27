# The oxi Ecosystem — Three-Plane Topology

> **Version:** v1.3 · **Date:** 2026-08-27 · aligned to `ARCHITECTURE.md` v2.11
> **Status:** Canonical for *how the oxi apps compose* and the order in which that happens.
> Per-app internals remain canonical in each app's own docs.
> **Companion:** `doc/ARCHITECTURE.md` (oxibrain itself). For the per-app public surface
> that oxi apps depend on, see `doc/CONSUMPTION_CONTRACT.md` — this file does not restate
> unstable API details.
> **Supersedes:** v1.2 (2026-08-23) — v1.3 carries the v2.11 daemonless two-plane
> cutover end-to-end: no daemon, no listening socket, no `serve --daemon`; the data
> plane is reached via `Brain::open` (embedded) or a caller-owned `serve --stdio`
> child; documents live in a disposable `documents.db` cache rebuilt from
> `documents.toml`-configured files and gix read history. C1–C3 and C6–C7 unchanged;
> C4, C5's tree, and C8 are rewritten. §3.5/§3.6 host notes updated accordingly.

---

## 0. TL;DR

Three planes, one cross-plane contract, no plane owns another.

| Plane | Owner | Verb | Durable state? |
|---|---|---|---|
| **Foundation contract** (`~/.oxi/foundation/v1/`) | the user; read by every host | *describe providers and packages* | non-secret by construction |
| **oxibrain durable data plane** (`~/.oxi/brain/`) | the `oxibrain` binary — embedded (`Brain::open`) or a caller-owned `serve --stdio` / foreground `serve --http` child | *remember and understand* | yes — the only durable-memory store in the ecosystem |
| **oxios orchestration / experience plane** | `oxios` (runtime) and consumers (`oxicode`, `oxiline`, `oximemo`, third-party MCP clients) | *run agents, capture, manage time* | host-owned; the brain is *advisory* here |

The single organizing rule is unchanged from v0.2 and is now stated at the plane level:

> **Each plane keeps its own source of truth. Adjacent planes are queried, never overwritten.**

That is what keeps the brain shared infrastructure without making it a single point of
failure: with no brain child running, oximemo still captures, oxiline still runs the day, and

---

## 1. The three planes

```
┌────────────────────────────────────────────────────────────────────────────┐
│  Foundation contract                                                       │
│  ~/.oxi/foundation/v1/  profiles.json  packages.lock                        │
│  ── non-secret, schema-versioned, version-pinned ────────────────          │
└────────────────────────────────────────────────────────────────────────────┘
              ▲                                             ▲
              │  reads locator (Keychain service/account)   │  capability request
              │                                             │  (workspace.*, brain.query, ...)
              │                                             │
┌─────────────┴──────────────────────────────────────────────────────────────┐
│  oxibrain durable data plane                                                │
│  ~/.oxi/brain/{brain.db, documents.db, documents.toml}                      │
│  Brain::open / serve --stdio (caller-owned) / serve --http (loopback)      │
│  ── sole durable-memory store; operation-scoped writer; ledger + cache ──  │
└─────────────┬─────────────────────────────────────────────────────────────┘
              ▲                ▲                ▲                  ▲
              │ MCP/RPC        │ MCP/RPC        │ MCP/RPC          │ MCP/RPC
              │                │                │                  │
┌─────────────┴───────┐ ┌──────┴───────┐ ┌──────┴────────┐ ┌──────┴──────────┐
│  oxios             │ │  oxiline     │ │  oximemo      │ │  external MCP   │
│  (orchestration    │ │  (time)      │ │  (capture /   │ │  clients        │
│   · experience)    │ │              │ │   documents)  │ │                 │
└────────────────────┘ └──────────────┘ └───────────────┘ └─────────────────┘
                              ▲                ▲                ▲
                              │                │                │
                       ┌──────┴────────────────┴────────────────┘
                       │  oxicode  (agent SDK; supplies `oxicode-ai`
                       │           LlmPort adapter; profile-resolved)
                       └───────────────────────────────────────────
```

### 1.1 Foundation contract plane

Lives in `~/.oxi/foundation/v1/`. Two files, both **non-secret by construction**:

- `profiles.json` — provider profiles, each with a `{service, account}` Keychain locator
  for the secret and a `roles` list (`memory.extract`, `memory.consolidate`,
  `coding.primary`, `assistant.general`).
- `packages.lock` — resolved Foundation packages with `name`, `version`,
  `digest: sha256-<hex>`, `source`, `trust`, `targets`, and abstract `requirements`
  drawn from `workspace.read`, `workspace.patch`, `shell.execute`, `browser.navigate`,
  `brain.query`, `schedule.manage`.

No executable. No daemon. The contract is parsed by each host independently against a
shared fixture corpus (`tests/fixtures/oxi-foundation/v1/`). The detail, JSON shapes,
Keychain-locator rules, and precedence are pinned in `doc/spec/oxi-foundation-v1.md`;
the rationale for "schema contract, not runtime crate" is in `doc/adr/ADR-007`.

### 1.2 oxibrain durable data plane

One binary, three entry points, **no daemon and no listening socket**. The plane is
reached either **embedded** — a host process calls `Brain::open(BrainConfig::at(...))`
and holds a handle-free facade (cheap `Clone` of config + ports; no store actor,
no writer thread) — or **as a caller-owned child**: `oxibrain-client` builds a
`LocalProcessEndpoint { executable, dir }` and spawns `oxibrain serve --stdio --dir
<dir>`; the child speaks JSON-RPC over its stdin/stdout and dies when stdin closes.
`oxibrain serve --http <addr>` is the foreground loopback variant for the
operations console (`ARCHITECTURE.md` §16.6). The on-disk shape is two databases:
`brain.db` (memory ledger + projection + ops, schema v11) and `documents.db`
(disposable document cache, schema v1), plus `documents.toml` (configured document
roots — `[[root]]` rows with `alias`, `path`, `space`, `include`/`exclude`,
`max_file_bytes`). The CLI is canonical in `doc/ARCHITECTURE.md` §16.4; the public
Rust facade and the MCP tool surface are canonical in `doc/ARCHITECTURE.md` §16.1–§16.2
and `doc/CONSUMPTION_CONTRACT.md` — this document does not restate them.

Hosts reach the plane via `oxibrain-client@0.8.x`, which pairs every connection with
`spawn_local` (or `spawn_local_with_token` on scoped sessions). Discovery is gone:
there is no default-path lookup, no `$OXIBRAIN_SOCKET`, and no ambient endpoint.
Capability negotiation rides the `handshake` JSON-RPC method that the client calls
immediately after spawning the child — metadata only, never a replacement for a
token or a `Scope` check. The MCP tool surface stays at fifteen tools; native
JSON-RPC methods (`handshake`, `reproject`, `spaces/list`, `document_history`)
extend the surface without counting against the cap.

### 1.3 oxios orchestration / experience plane

The consumers. oxios runs agent sessions; oxiline owns time-shaped state; oximemo owns
the vault; third-party MCP clients connect with the same `oxibrain-client`. oxicode is
the agent SDK; it ships an `oxicode-ai` `LlmPort` adapter that resolves a profile from
the Foundation contract before it asks `oxibrain-client` for anything else. A consumer
that cannot parse a profile still works — the local-GGUF default (`oxibrain-llm-local`)
needs no Foundation input.

The plane owns its own source of truth. The brain is **advisory** here: a host's
`search` call returns `{ memory, documents, freshness }` — document hits are a
disposable cache the brain rebuilds from the host's files; memory hits are the
ledger. An `assemble_context` call returns material for a prompt; what the
consumer does with the material is the consumer's call.

---

## 2. Contracts between the planes

These are binding. An integration that breaks one is wrong even if it works.

### C1 — The brain is additive, never load-bearing

Every consuming app retains its primary function with oxibrain absent. oximemo
captures to files; oxiline runs the day; oxios agents execute without memory.
Integrations degrade to a disabled panel, never to a blocked action or a spinner.
**Test it: each app's CI runs its main flow with no brain reachable.**

### C2 — One space, many sources

Spaces are privacy boundaries (personal / work / a client), **never app boundaries**. All
consumers write into the same space with different `SourceRef` labels. Partitioning by
app rebuilds the silos the brain exists to remove — the entire point is that a Tuesday
routine, a note from March, and yesterday's agent session can be seen to concern the
same entity.

### C3 — Files are edited by their owner, ingested by the brain

oxibrain never writes into a user's vault. It reads through a connector. Annotations it
wants to surface (contradictions, suggested links, entity mentions) are returned through
the API and rendered by the owning app — they are not written into the user's files.

### C4 — Document revisions live in consumer-owned git, not in the ledger

A note's edit history is the user's git history. Mutations to a vault file are
committed by the owning app (oximemo / oxios) — the brain never writes a repo
(ADR-011, `oxi-vault-git` is consumer-owned). What the brain keeps in
`documents.db` is a **disposable cache**: a manifest of currently-known files,
FTS + vector indices, and the per-file `rev → committed_at → content` snapshots
needed for `Brain::document_history` and the `doc://<alias>/<locator>?rev=<rev>`
resource. `documents.db` is rebuildable from `documents.toml`-configured files and
gix HEAD history by `oxibrain index --documents`; dropping and rebuilding it loses
no user data. Memory is the only ledger — documents never become memory episodes
anymore — so "when did I change my mind about this?" is answered by reading
revision history, not by replaying an occurrence chain.

### C5 — One installation root, one owner per subtree

```
~/.oxi/
├── config.toml                  # shared: which brain, which space, [vault] path/space
├── foundation/v1/               # Foundation contract — non-secret, every host reads
│   ├── profiles.json
│   └── packages.lock
├── brain/                       # oxibrain data — operation-scoped writer (embedded Brain
│   │                            #  facade, caller-owned serve --stdio, or foreground
│   │                            #  serve --http); one writer per store per operation
│   ├── brain.db                 # memory ledger + projection + ops (schema v11)
│   ├── brain.lock               # advisory lock on brain.db
│   ├── documents.db             # disposable document cache (schema v1)
│   ├── documents.lock           # advisory lock on documents.db
│   └── documents.toml           # configured document roots — [[root]] rows
│                                #  (alias, path, space, include/exclude, max_file_bytes)
└── vault/                       # SHARED USER FILE SPACE (oxios + oximemo write;
    │                            #  oxi-frontmatter contract governs; see disciplines below)
    ├── oximemo.toml             # vault config (owned by oximemo)
    ├── <folder>/<slug>.md       # user memos (frontmatter required)
    ├── Chat.md, Later.md, …     # oxios app files (BodyOnly — no frontmatter)
    └── _assets/, .trash/, .git/ # app machinery (oximemo) + shared git history (oxi-vault-git, oximemo + oxios)
```

One root, one config file, two databases — `brain.db` and `documents.db` each
admit exactly one writing process at a time (`brain.lock`, `documents.lock`).
**Owned subtrees keep exactly one writer class:** the `oxibrain` binary writes
`brain/` and nothing else (via short operation-scoped handles); hosts never write
`foundation/v1/`. The `vault/` subtree is a **shared user file space** — multiple
apps may write into it, and three disciplines make that safe:

1. **Every write goes through the `oxi-frontmatter` contract.** Atomic
   tmp+fsync+rename, a single frontmatter block, unknown-key- and app-table-preserving
   serialization. Direct `fs::write` to a vault `.md` is a contract violation.
2. **No derived state inside `vault/`.** No app places an index, cache, or lock file
   inside the vault — derived state lives in each app's own support directory. The
   vault is files, frontmatter, and git; everything else is rebuildable from that.
3. **Per-file last-writer-wins, cross-app visibility by frontmatter.** When two apps
   touch the same file, last-writer-wins resolves the conflict; cross-app visibility
   for memo indexing is by frontmatter convention (a `---` block ⇒ memo, no block ⇒
   `BodyOnly`), never by reserved filenames.

**Vault resolution, as shipped.** `~/.oxi/config.toml` `[vault].space` is the
ecosystem-canonical brain space — both apps read it for vault ingestion, and a
per-app space setting only applies when the ecosystem key is absent.
`[vault].path` is honored by oxios (tier 2 of `kernel.knowledge_root` →
`[vault].path` → `~/.oxi/vault`) and by the ecosystem migration tooling; oximemo
in this release does not read it — it opens the default `~/.oxi/vault`, with a
custom root set per-app via `--vault` or `OXIMEMO_VAULT`. Per-app overrides can
therefore **diverge silently today** — a split pair fragments the vault: a second
space registration leaves one space with a single full pass and no watcher.
Operators running custom roots must keep both apps on one tree; a loud cross-app
mismatch warning is future work. Apps reach the brain by spawning a `serve --stdio`
child against an explicit `--dir` (`oxibrain-client::spawn_local(LocalProcessEndpoint
{ executable, dir })`) — there is no ambient discovery, no default path, and no
`$OXIBRAIN_SOCKET`. A fresh install still finds the existing brain with no setup:
the agreed default directory is `~/.oxi/brain`, and every host and consumer app is
expected to pass it explicitly.

### C6 — Integration is a client dependency, never a fork

Apps depend on `oxibrain-client` (thin, stable, semver'd). Nobody links `oxibrain-core`.
Nobody opens the store file directly. Target integration cost: **under 200 lines per app.**
If an integration is bigger than that, the missing capability belongs in the brain.

### C7 — Profiles carry locators, never secrets

The Foundation contract never carries a secret. A profile's `credential` field is a
`{service, account}` OS-Keychain locator; the secret stays in the Keychain. A profile
that includes `api_key`, `bearer`, `access_token`, or `refresh_token`-shaped fields is
rejected at parse time. Environment variables remain an explicit development /
automation override, never the Foundation path.

### C8 — Transport is caller-owned stdio; auth-first-message is preserved

Every connection to the data plane is a **caller-owned child process** —
`oxibrain serve --stdio --dir <dir>` — built by `oxibrain-client` from a
`LocalProcessEndpoint { executable, dir }`. There is no listening socket, no
`default_socket_path`, no `connect_default`, no `connect_endpoint`, and no
`BrainEndpoint`: every caller passes an explicit `dir`, every connection is one
child process, and the child dies with its stdin. Immediately after the spawn the
client sends a `ClientHello` and receives a `ServerInfo` carrying the child's
`schema_version`, `server_version`, and supported features — used for capability
negotiation. The existing token-before-payload auth rule and `Scope`/`Capability`
semantics from `ARCHITECTURE.md` §15.1–§15.2 are unchanged. Handshake metadata
never replaces a token and never broadens scope.

---

## 3. Per-app position

### 3.1 oximemo — capture and write (experience plane)

Card-based memo app for macOS. Plain `.md` + YAML-subset frontmatter as the source of
truth; `redb` metadata index; `tantivy` BM25; GUI/CLI parity. It remains the ecosystem's
authoring interface.

Two guardrails stay:

1. **The capture path is inviolable.** Note mode may not add one millisecond to
   `Option`×2 → overlay → save. The ≤16 ms budget is CI-measured, not a past
   achievement.
2. **The "no AI" promise survives.** oximemo still contains no model, no prompt, no
   embedding. Intelligence arrives from outside — over a `serve --stdio` child
   spawned by `oxibrain-client`, or via the user-activated delegated agent CLI
   (see the copilot amendment below) — always in a panel the user can close.

**Copilot delegation (2026-08-23 amendment, RFC-050-style):** oximemo may additionally
act as a **selective dispatcher for an external terminal-agent CLI the user has
explicitly activated** (v1: `oxios run --json --session`; any verified non-interactive
CLI contract qualifies). This does not weaken either guardrail: the capture path is
untouched, oximemo authors no instruction text (it hands the agent a declarative
context block plus a pointer to the deployed `SKILL.md` contract), and the turn runs
as one subprocess whose approvals, sandboxing, and providers remain the chosen
agent's own policy — oximemo never attaches permission-bypass flags. Vault writes
follow the frontmatter contract and the agent's policy; oximemo labels observed
changes without claiming causality. Agent discovery never runs on the app-startup or
capture paths, and the whole surface hides when no agent is activated (C1).

**Brain integration:** document cache (read-only gix history of the vault; a
`serve --stdio` child calls `Brain::document_history`). Panels: related notes,
contradictions, entities mentioned, "you wrote about this before". All read-only,
all closable, all degrade to absent when no brain child is running (C1).

### 3.2 oxiline — manage time (experience plane)

Routine/day-management, "time as a playhead", Rust core + Tauri v2, CLI-first. Owner of
everything time-shaped in the ecosystem.

**Brain integration:** writes `Event` episodes (routine completions, schedule changes)
— a stream nothing else in the ecosystem produces, and the one that makes questions
like "since when have I done this every Tuesday?" and "what was I doing the week that
project stalled?" answerable. Reads timelines back.

### 3.3 oxios — run agents (experience plane, with orchestration responsibilities)

Agent OS — agent runtime, sessions, tools, MCP client, single binary with an embedded
web UI. After the M5 migration it has no memory code of its own; agents call
`assemble_context` per turn.

**Brain integration:** the heaviest. Writes `Conversation` and `AgentTrace` episodes;
reads `assemble_context` on every turn. Latency matters here in a way it does not
elsewhere — hence the §13.2 target of < 150 ms for a 3K-token context assembly. **This
is the integration where the brain outage risk is sharpest** (`ADR-002`); with no
in-process memory of its own, oxios agents lose memory entirely when no `serve
--stdio` child is up.

### 3.4 oxibrain — remember and understand (data plane)

Covered by `ARCHITECTURE.md`. Its ecosystem-facing obligations:

- Ship `oxibrain-client` before asking any app to integrate.
- Never require an app to change its storage.
- **Never require an API key, an account, or a second install to be useful.** oxibrain
  ships its own model (`ARCHITECTURE.md` §8, C2); MCP client sampling and HTTP providers
  are optional quality tiers, never the path to a working product.
- **Never make quality depend on the user's language** (`ARCHITECTURE.md` §7, C3). An
  ecosystem app must be able to ship internationally without asking what the brain
  supports.
- Stay independently valuable: someone who uses none of the other apps must still get
  a complete second brain from `cargo install oxibrain-cli`. If that ever stops being true,
  the brain has degenerated into oxios's memory library.

### 3.5 oxicode — agent SDK (Foundation consumer)

`oxicode` ships a Foundation host: it parses `profiles.json`, resolves a Keychain
locator through a `SecretResolver` at its CLI/facade boundary, and wires the result
into an `LlmPort` adapter (`oxicode-ai`). Two follow-ups live outside this repo and are
not on the oxibrain critical path:

- **Spawn defaults.** The current `oxicode` default (`executable`, `dir`) does
  not yet match the agreed `~/.oxi/brain` directory, so its `spawn_local`
  callers pass an explicit `LocalProcessEndpoint` until the default lands.
  Tracked in oxicode.
- **Memory backend.** The current `oxicode` MCP `memory.*` family does not map 1:1 to
  oxibrain's native `ingest` / `search` / `remember` / `retract` tools (which use the
  `space` argument). Tracked in oxicode.

oxicode never opens `oxibrain`'s store file directly; it goes through `oxibrain-client`.

### 3.6 oxios — Foundation host status

`oxios` also ships a Foundation host; its parser dialect does not yet match the v1
frozen shapes — alignment is a tracked follow-up in oxios, not in oxibrain. Bootstrap
today is probe-only — its `spawn_local` hardcodes a `dir` it discovers by walking
`$OXI_BRAIN_DIR` → `~/.oxi/brain`, and does not yet negotiate `ClientHello` /
`ServerInfo`. Tracked in oxios, not in oxibrain.

### 3.7 The rest

| Project | Relationship |
|---|---|
| `oxibrowser` | contributes web-clip episodes at the `Untrusted` trust tier — the tier exists partly for this. |
| `oxibuilder` | web platform, out of scope. May consume the brain over HTTP later. |
| marketing / sites | unaffected. |

This repo does **not** track per-host roadmaps beyond the brief status notes above; the
hosts own their own sequencing.

---

## 4. Where to look next

| Question | Source |
|---|---|
| What is oxibrain's architecture? | `doc/ARCHITECTURE.md` |
| What is oxibrain's public surface and stability contract? | `doc/CONSUMPTION_CONTRACT.md` |
| What is the on-disk shape of Foundation v1? | `doc/spec/oxi-foundation-v1.md` |
| Why is the Foundation a schema contract, not a runtime crate? | `doc/adr/ADR-007-oxi-foundation-contract.md` |
| What sequence is the oxibrain repo in? | `doc/ROADMAP.md` |
| What is the oxios / oxicode / oxiline host status? | the host's own repository |
