# The oxi Ecosystem — Three-Plane Topology

> **Version:** v1.0 · **Date:** 2026-08-17 · aligned to `ARCHITECTURE.md` v2.5
> **Status:** Canonical for *how the oxi apps compose* and the order in which that happens.
> Per-app internals remain canonical in each app's own docs.
> **Companion:** `doc/ARCHITECTURE.md` (oxibrain itself). For the per-app public surface
> that oxi apps depend on, see `doc/CONSUMPTION_CONTRACT.md` — this file does not restate
> unstable API details.
> **Supersedes:** v0.2 (2026-08-11) — the v0.2 four-app / one-brain framing remains a
> useful narrative, but the topology now names three explicit planes and the Foundation v1
> contract that connects them.

---

## 0. TL;DR

Three planes, one cross-plane contract, no plane owns another.

| Plane | Owner | Verb | Durable state? |
|---|---|---|---|
| **Foundation contract** (`~/.oxi/foundation/v1/`) | the user; read by every host | *describe providers and packages* | non-secret by construction |
| **oxibrain durable data plane** (`~/.oxi/brain/`) | the `oxibrain` daemon — sole writer | *remember and understand* | yes — the only durable-memory store in the ecosystem |
| **oxios orchestration / experience plane** | `oxios` (runtime) and consumers (`oxicode`, `oxiline`, `oximemo`, third-party MCP clients) | *run agents, capture, manage time* | host-owned; the brain is *advisory* here |

The single organizing rule is unchanged from v0.2 and is now stated at the plane level:

> **Each plane keeps its own source of truth. Adjacent planes are queried, never overwritten.**

That is what keeps the brain shared infrastructure without making it a single point of
failure: if the daemon is down, oximemo still captures, oxiline still runs the day, and
oxios agents still execute — with worse memory, not with no function.

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
┌─────────────┴─────────────────────────────────────────────┴────────────────┐
│  oxibrain durable data plane                                                │
│  ~/.oxi/brain/oxibrain.sock          oxibrain serve --daemon               │
│  ── sole durable-memory store; sole writer; ledger + projection ──        │
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

The `oxibrain` daemon, default listening socket `~/.oxi/brain/oxibrain.sock`
(`$OXIBRAIN_SOCKET` override; `serve --daemon` binds the default when `--socket` is
absent). It is the **only** durable-memory store in the ecosystem. Its public surface
— the Rust facade, the MCP tool surface, the CLI — is canonical in
`doc/ARCHITECTURE.md` and `doc/CONSUMPTION_CONTRACT.md`; this document does not restate
them.

Hosts reach the plane via `oxibrain-client`, which on top of the existing JSON-RPC
surface exposes additive planned helpers (`default_socket_path`, `connect_default`,
`connect_endpoint`, `ClientHello`, `ServerInfo`) — pinned to land in
`oxibrain-client@0.3.x`, not yet shipped in `0.2.0`. The MCP tool surface stays at
fifteen; capability negotiation rides the transport handshake, not a sixteenth tool.

### 1.3 oxios orchestration / experience plane

The consumers. oxios runs agent sessions; oxiline owns time-shaped state; oximemo owns
the vault; third-party MCP clients connect with the same `oxibrain-client`. oxicode is
the agent SDK; it ships an `oxicode-ai` `LlmPort` adapter that resolves a profile from
the Foundation contract before it asks `oxibrain-client` for anything else. A consumer
that cannot parse a profile still works — the local-GGUF default (`oxibrain-llm-local`)
needs no Foundation input.

The plane owns its own source of truth. The brain is **advisory** here: the connector
that watches a vault turns file changes into episodes; an `assemble_context` call returns
material for a prompt; what the consumer does with the material is the consumer's call.

---

## 2. Contracts between the planes

These are binding. An integration that breaks one is wrong even if it works.

### C1 — The brain is additive, never load-bearing

Every consuming app retains its primary function with the daemon stopped. oximemo
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

### C4 — An edit is a new episode, not an update

When a note changes, the connector writes a **new episode** (new content hash) rather
than mutating the old one. The ledger therefore records how a note evolved, which is
what makes "when did I change my mind about this?" answerable. Debounce and a
minimum-diff threshold keep this from becoming version spam; consolidation compacts old
revisions (`ARCHITECTURE.md` §13).

### C5 — One installation root, one writer per subtree

```
~/.oxi/
├── config.toml                  # shared: which brain, which space, provider settings
├── foundation/v1/               # Foundation contract — non-secret, every host reads
│   ├── profiles.json
│   └── packages.lock
└── brain/                       # oxibrain store — daemon is the sole writer
    └── oxibrain.sock            # default listening socket
```

One root, one config file, one daemon. Each subtree has exactly one writer. Apps
discover the brain by convention (`~/.oxi/brain/oxibrain.sock` or `$OXIBRAIN_SOCKET`),
not configuration, so a fresh install of any app finds the existing brain with no setup.

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

### C8 — Discovery is additive and auth-first-message is preserved

`oxibrain-client` exposes `default_socket_path()` (returning `~/.oxi/brain/oxibrain.sock`
or `$OXIBRAIN_SOCKET`), `connect_default()`, and `connect_endpoint(...)`. Hosts speak a
`ClientHello` and receive a `ServerInfo` carrying the daemon's `schema_version`,
`server_version`, and supported features — used for capability negotiation. The
existing token-before-payload auth rule and `Scope`/`Capability` semantics from
`ARCHITECTURE.md` §15.1–§15.2 are unchanged. Discovery metadata never replaces a token
and never broadens scope.

---

## 3. Per-app position

### 3.1 oximemo — capture and write (experience plane)

Card-based memo app for macOS. Plain `.md` + TOML frontmatter as the source of truth;
`redb` metadata index; `tantivy` BM25; GUI/CLI parity. It remains the ecosystem's
authoring interface.

Two guardrails stay:

1. **The capture path is inviolable.** Note mode may not add one millisecond to
   `Option`×2 → overlay → save. The ≤16 ms budget is CI-measured, not a past
   achievement.
2. **The "no AI" promise survives.** oximemo still contains no model, no prompt, no
   embedding. Intelligence arrives over a socket from the brain, always in a panel the
   user can close.

**Brain integration:** vault connector (watch → episode). Panels: related notes,
contradictions, entities mentioned, "you wrote about this before". All read-only, all
closable, all degrade to absent when the daemon is down (C1).

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
in-process memory of its own, oxios agents lose memory entirely when the daemon is down.

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

- **Socket default.** The current `oxicode` default does not match
  `~/.oxi/brain/oxibrain.sock`. Tracked in oxicode.
- **Memory backend.** The current `oxicode` MCP `memory.*` family does not map 1:1 to
  oxibrain's native `ingest` / `search` / `remember` / `retract` tools (which use the
  `space` argument). Tracked in oxicode.

oxicode never opens `oxibrain`'s store file directly; it goes through `oxibrain-client`.

### 3.6 oxios — Foundation host status

`oxios` also ships a Foundation host; its parser dialect does not yet match the v1
frozen shapes — alignment is a tracked follow-up in oxios, not in oxibrain. Bootstrap
today is probe-only — it discovers the daemon by the default socket path but does
not yet negotiate `ClientHello`/`ServerInfo`. Tracked in oxios, not in oxibrain.

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
