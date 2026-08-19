# Ecosystem v2 — Verb Ownership and the oxibrain Console

> **Version:** v1.1 · **Date:** 2026-08-19
> **Status:** Design blueprint. Canonical for *which surface owns which human verb*.
> On acceptance this supersedes `doc/ECOSYSTEM.md` v1.0 §3 and amends
> `doc/ARCHITECTURE.md` §1.3 (the "desktop app" delivery shape).
> **Companions:** `doc/adr/ADR-008-console-technology.md` (console tech choice),
> `doc/spec/brain-ui-v2.md` (current console implementation), `doc/ROADMAP.md`.
> **Method:** every claim about the current state below was verified against source
> at the cited path. Where the shipped code contradicts an existing doc, the code wins
> and the contradiction is called out.
> **v1.1 revision:** v1.0's D-1 ("oxios's KnowledgeBase vault is an orphan") was **false** —
> `KnowledgeLens` already ingests it into the daemon. D-1 is replaced by the verified and
> more serious defect it was masking: every MCP ingest is stamped `SourceRef::Note` +
> `TrustTier::Trusted`. §4.2 accordingly no longer proposes deleting the oxios editor.

---

## 0. TL;DR

`ECOSYSTEM.md` v1.0 organised the ecosystem by **plane** (contract / data / experience).
Planes describe *where state lives*. They do not say *who is allowed to do what*, and
that is the question actually causing duplication. This blueprint reorganises by **verb**.

> **One rule: every human verb has exactly one owning surface. A second implementation
> of an owned verb is a bug, not a convenience.**

Six verbs, three owners:

| Verb | Owner | Why it cannot move |
|---|---|---|
| **Capture** — dump a passing thought | oximemo (global hotkey) | must survive a dead daemon; must be instant |
| **Author** — write a durable knowledge document | oximemo vault (`.md` / `.html`) | needs an editor, links, templates, files |
| **Ask** — question memory in natural language | oxios agents · CLI `ask` | it is agent work; a second chat is a worse chat |
| **Observe** — graph, timeline, what is believed and why | **oxibrain console** | it is about the *projection*, not about files |
| **Curate** — merge, retract, declare, redact, predicates | **oxibrain console** · CLI | requires modelling merges/statements/predicates — past C6's 200-line host budget |
| **Operate** — reproject, re-extract, models, spaces, tokens, backup | **CLI** primary · console secondary | admin of the store |

Capture / Author / Ask are **host** verbs. Observe / Curate / Operate are **brain** verbs.
The brain needs a surface because three verbs have no possible host implementation — not
because a graph is nice to look at.

---

## 1. Verified current state

### 1.1 The three human surfaces

| Surface | Delivery | Reads brain | Writes brain | Authors durable text |
|---|---|---|---|---|
| `oxibrain/apps/brain-ui` | **not delivered** — hand-run `vite build` + `serve --ui-dir`; no CI build step, no binary embedding | 8 routes | **yes** — `remember`→Primary, `retract`→Declaration, `merge_entities`→Declaration | no |
| `oxios` embedded web UI | embedded in the oxios binary | `/brain`, 4 read-only tabs | panel: **no**. Writes exist via CLI `oxios brain ingest` and the agent `memory_write` tool (`brain.remember`) | **yes — its own KnowledgeBase markdown vault** |
| `oximemo` desktop (Tauri v2) | signed, released | `brain_status` + `brain_gather` (stats + recall) | **no** | **yes — `.md` / `.html` vault** |

Evidence: `crates/oxibrain-cli/src/cli.rs:153-156` (`--ui-dir`),
`crates/oxibrain-mcp/src/server.rs:1620-1656` (static file read, no embedding),
`.github/workflows/ci.yml` (cargo only, no bun step),
`apps/brain-ui/.gitignore` (`dist/` ignored),
`apps/brain-ui/src/views/{CaptureView,ConflictsView,MergesView}.tsx` (the three writes),
oxios `src/api/routes/knowledge_routes.rs:648-665` (`PUT /api/knowledge/file/{path}` →
`knowledge.note_write`), oxios `web/src/hooks/use-brain.ts` (all GET plus read-only POST
`recall`), oximemo `apps/desktop/src-tauri/src/lib.rs:724-776` (`brain_status`,
`brain_gather`).

### 1.2 Four defects, all structural

**D-1 — every MCP ingest is stamped `SourceRef::Note` + `TrustTier::Trusted`, and episodes
are immutable.** Both write paths any host can reach — the `ingest` and `remember` MCP
tools (`crates/oxibrain-mcp/src/server.rs:448-470`, `:656-672`) — call
`brain.ingest_note(...)`, and `ingest_note_impl`
(`crates/oxibrain/src/ingest.rs:32-33`) hardcodes `source: SourceRef::Note { path }` and
`trust: TrustTier::Trusted`. `TrustTier` appears **zero** times in the whole MCP server.

Consequences:

- An oxios agent's raw scratch note (`author: "agent"`, `quality: Raw`,
  `needs_review: true` — oxios `crates/oxios-kernel/src/tools/builtin/knowledge_tool.rs:169-213`)
  lands with exactly the trust of a human-authored oximemo document.
- `SourceRef::AgentTrace`, `Conversation`, and `Event` are unreachable over the wire, so
  `ECOSYSTEM.md` C2 ("one space, many sources, different `SourceRef` labels") does not
  actually hold — every source collapses to `Note`.
- `ECOSYSTEM.md` §3.7's promise that `oxibrowser` clips arrive at `Untrusted` is
  unimplementable through the only ingest API a host has.
- P10's uncertainty-from-trust-exclusions cannot compute anything, because nothing is ever
  excluded.

**This is the most urgent defect in the ecosystem, and it is the only one that is not
repairable later.** `episodes.trust` is a column on an append-only row (`insert_episode`,
`crates/oxibrain-store/src/ledger.rs:83-103`); reprojection rebuilds the projection from
episodes, so it cannot fix an episode's own provenance. Every episode ingested while this
is unfixed is mislabelled permanently.

*Superseded claim:* an earlier draft asserted that oxios's KnowledgeBase vault was an
orphan never ingested into oxibrain. **That was false.** `KnowledgeLens` ingests every
vault file change into the daemon — oxios
`crates/oxios-kernel/src/kernel_handle/knowledge_lens.rs:324-359` → `index_to_brain` →
`brain.remember(content, source = "knowledge:lens:{path}")` →
`crates/oxios-kernel/src/brain/mod.rs:121-127` → `client.ingest(...)`, wired live at
oxios `src/kernel.rs:126` and `src/main.rs:1537`. The vault is connected. What is wrong is
the *provenance* it arrives with, which is D-1.

**D-2 — curation is unreachable by any human.** The CLI cannot curate:

- `EntityCmd` (`crates/oxibrain-cli/src/cli.rs:233-239`) contains **only `Show`**. Its own
  doc comment claims `entity show|merge|split|alias` — `merge`, `split`, and `alias` do
  not exist.
- `PredicateCmd` (`cli.rs:227-230`) contains **only `List`**. No `add`.
- `Retract` and `Declare` appear **zero times** in `cli.rs`.

So `merge_entities`, `retract`, and `declare` are reachable only as MCP tools, and the only
human MCP client is the console — which is not delivered. Entity resolution without a
human review queue silently accumulates bad merges. **This is a correctness defect, not a
UX defect.**

**D-3 — the vault watcher does not exist.** `ECOSYSTEM.md` v1.0 §3.1 describes a "vault
connector (watch → episode)". There is no watcher: `notify` appears in **zero**
`Cargo.toml` files, and `Command::Sync` (`cli.rs:31-39`) is a one-shot idempotent
directory scan. The capture → ingest → distil loop is not closed; a human or a timer must
run `oxibrain sync` by hand.

**D-4 — the console cannot address a space.** Every console call hardcodes
`space="personal"` (`apps/brain-ui/src/api.ts:95,127,131,153`;
`views/GraphView.tsx:45`). Spaces are the privacy boundary (`ECOSYSTEM.md` C2). A surface
that cannot switch spaces cannot be the administrative surface of a multi-space brain.

### 1.3 What is *not* a defect

Three surfaces reading brain state is legitimate, because they answer three different
questions:

- oximemo's `BrainPanel` — "what does the brain know **about the note I am looking at**"
- oxios's `/brain` — "what does the brain know **relevant to this session**"
- the console — "what does the brain know, **period**"

Each is cheap inside its own host. What is illegitimate is a second merge queue.

`.html` vault ingestion is also no longer a gap: `crates/oxibrain-connectors/src/html.rs`
landed in `12bab11 feat(connectors): scan html notes from oximemo vaults`.

**Two markdown vaults are also legitimate — they have different authors.** oximemo's vault
is human-authored durable notes. oxios's `<workspace>/knowledge` (default
`~/.oxios/workspace/knowledge`, oxios `crates/oxios-kernel/src/config.rs:1422-1428`) is
predominantly **agent-written**: the `knowledge` agent tool, the autonomous
`PersistenceHook` (oxios `crates/oxios-kernel/src/persistence_hook.rs:120-258`), and a
background curation task all write it; the human web editor is a **review and repair
surface** for notes the agent stamped `needs_review: true`, not a rival authoring app.
Agents also *read* it back into their prompts (oxios
`crates/oxios-kernel/src/agent_runtime.rs:483-505` via `recall_for_context`).

So the two vaults are one human notebook and one agent scratchpad. The duplication that
does exist is at the *engine* level — two markdown engines with wikilinks and backlinks,
`oximemo-core` and `oxios-markdown` (~6–7k LOC each) — which is a cost, not an
architectural error, because each engine's owner and lifecycle differ.

**Push and pull ingest can coexist safely.** `insert_episode` deduplicates on
`(space_id, content_hash)` and returns a no-op for a repeat
(`crates/oxibrain-store/src/ledger.rs:65-80`, "idempotency layer 1"). A file reaching the
ledger both by a host push and by a brain-side scan produces one episode, not two.

---

## 2. Target topology

```
                    authors here (owns the files)
   ┌──────────────────────────────────────────────┐
   │  oximemo — capture (Option×2) · author · contextual brain panel
   └────────────┬─────────────────────────────────┘
                │ writes .md / .html
                ▼
        ┌───────────────┐
        │  the vault    │  the ecosystem's ONE human-authored text store
        └───────┬───────┘
                │ daemon-owned watcher: debounce → new episode (C4)
                ▼
┌───────────────────────────────────────────────────────────────────┐
│  oxibrain daemon — ledger + projection · sole writer (P8)          │
│                                                                    │
│   ├── console  (browser app, served by the daemon, embedded)        │
│   │      observe · curate · operate                                │
│   └── CLI     (headless parity for every console write)             │
└───────────────┬───────────────────────────────────────────────────┘
                │ MCP / JSON-RPC
     ┌──────────┴──────────┬─────────────────────┐
     ▼                     ▼                     ▼
  oxios                oximemo panel      external MCP clients
  agents · ask         contextual read     (Claude Desktop, …)
  memory_write         only
```

There is **one** desktop application in the ecosystem (oximemo) and **one** console
(a browser app served by `oxibrain serve --ui`). No second Tauri app is built, signed,
notarised, or auto-updated.

---

## 3. The oxibrain console

### 3.1 Definition

> The console is the administrative and curatorial surface of the brain. It is not a
> reading app, not a chat, and not an editor.

### 3.2 Scope — in

| Route | Purpose | Writes |
|---|---|---|
| `/` Overview | space stats, health, extraction-failure count, conflict count | — |
| `/search` | find an entity or statement in order to act on it | — |
| `/entity/$id` | beliefs, provenance (`why`), timeline | — |
| `/neighborhood/$id` | ranked n-hop neighbours as a list; force-directed graph is a secondary toggle, not the primary view | — |
| `/merges` | **merge review queue** — candidates side by side, confirm / reject | `merge_entities` → Declaration |
| `/conflicts` | contradiction triage | `retract` → Declaration |
| `/predicates` | registry: list, and add | `predicate add` |
| `/failures` | extraction-failure inbox: inspect, re-extract one | triggers `extract` |
| `/operate` | reproject / re-extract progress, model status, doctor, token list, space switcher, and an **Ingest affordance** (paste text or drop a file → episode) | `ingest` |

### 3.3 Scope — out, and where it goes instead

| Removed | Goes to | Reason |
|---|---|---|
| `/ask` (`AskView.tsx`) | oxios agents · CLI `oxibrain ask` | question-answering wants history, streaming, follow-ups — that is a chat, and oxios owns chat. `AskView`'s useful half is search-to-navigate, which survives as `/search`. |
| `/capture` as a first-class route | oximemo's global hotkey; CLI `oxibrain ingest -`; the console's `/operate` Ingest affordance | a second capture habit competes with the hotkey. Demoting it keeps the standalone user unblocked without creating a rival capture UX. |
| any editor / file write | oximemo | `ARCHITECTURE.md` §1.4 |

### 3.4 Delivery — the single highest-value change

The console is currently a demo because nobody has it. Fix:

1. **Un-ignore and commit `apps/brain-ui/dist/`.** `cargo install oxibrain` cannot run
   `bun`, so the built bundle must be part of the published crate. Add it to the owning
   crate's `Cargo.toml` `include` list.
2. **Embed** with `include_dir!`, replacing the `tokio::fs::read` path at
   `crates/oxibrain-mcp/src/server.rs:1620-1656`.
3. **`oxibrain serve --ui`** serves the embedded bundle. `--ui-dir` survives as a
   development override.
4. **CI gates**, all three required:
   - `bun run build` then `git diff --exit-code apps/brain-ui/dist` — a committed bundle
     that does not match its source fails the build. This is what makes a committed
     artifact safe.
   - gzipped bundle size ≤ 400 KB (crates.io's package limit is 10 MB; the budget exists
     to keep `cargo install` honest, not to avoid the limit).
   - `grep` gate: zero occurrences of a hardcoded space literal in `apps/brain-ui/src`.

Acceptance: `cargo install --path . && oxibrain serve --http 127.0.0.1:18080 --ui` opens a
working console on a machine with no `bun`, no `node`, and no network.

### 3.5 Technology

**Keep React 19 + Vite. Embed the bundle.** Rejected: Leptos/Dioxus (WASM), egui,
Maud + HTMX. Full reasoning in `doc/adr/ADR-008-console-technology.md`.

The one-sentence version: "pure Rust" does not remove the build step (Leptos still needs
`wasm32-unknown-unknown` + `trunk` + `wasm-bindgen`), egui costs a second signed binary
and abandons the CSS design tokens shared with oximemo, and the genuinely pure-Rust
option — Maud + HTMX — would require introducing `axum` and rewriting the hand-rolled raw
TCP HTTP transport (`server.rs:1712 write_http_response`) that currently carries MCP.

---

## 4. Host contracts

### 4.1 oximemo — unchanged in kind, pinned in scope

- Keeps capture (`Option`×2 → overlay → save, ≤ 16 ms, CI-measured) and authoring.
- `BrainPanel` is **contextual, read-only, closable**. It may answer "what does the brain
  know about *this note*". It may never grow a general entity browser, a merge queue, or a
  retract button.
- Keeps its own wiki-link `GraphView` (`views/GraphView.tsx` reads the vault index, not
  oxibrain's entity graph — these are different graphs and both are legitimate).
- The vault is the ecosystem's single **human-authored** text store.

### 4.2 oxios — keep the editor, fix the provenance

1. **Keep the KnowledgeBase and its editor.** It is not a rival authoring app: the vault is
   predominantly agent-written and agent-read, and the human editor is the review surface
   for `needs_review: true` notes (§1.3). Deleting the write path while `KnowledgeLens`
   still ingests and `recall_for_context` still reads would leave agents able to read notes
   they can no longer correct — strictly worse than today.
   *Considered and rejected:* deleting the editor (an earlier draft's recommendation, based
   on the false premise that the vault was an orphan); and repointing it at the oximemo
   vault, which would put two engines with different wiki-link and frontmatter semantics
   over one store.
2. **Declare the vault's role in the ledger.** `KnowledgeLens` currently ingests through
   `remember`, so agent-written raw notes arrive `SourceRef::Note` + `Trusted` (D-1). Once
   the ingest API carries provenance (§6.1), the lens must pass the note's own frontmatter:
   `author`, `source`, `quality`, `needs_review`. A note the agent marked `Raw` and
   unreviewed must not enter the ledger at human-document trust.
3. **Pin `/brain` to session context.** The four tabs stay read-only and stay scoped to
   "relevant to this session". General browsing is the console's.
4. **Fix or remove `oxios brain curate`.** It is a stub that prints and
   `std::process::exit(0)` (oxios `src/main.rs:1630-1640`) while the real engine
   (`knowledge_curation.rs`) only runs as a background task that is disabled by default.
   The review loop the vault's `needs_review` flag exists to serve does not currently run.
   Tracked in oxios; listed here because the brain's trust story (§6.1) depends on it.

`memory_write` (agent-initiated `brain.remember`) stays. An agent writing memory is
ingest, not curation.

### 4.3 External MCP clients

Unchanged. They get the same fifteen-tool surface, capability-scoped. A third-party client
*may* build its own console; that is the point of the protocol. oxibrain ships one so that
"no other app installed" is still a complete product.

---

## 5. Closing the ingest loop

The watcher (D-3) is **daemon-owned**:

- Config gains `[[vault]]` entries: `path`, `space`, optional `include`/`exclude` globs.
- `oxibrain serve --daemon` watches each path with `notify`, debounces, and reuses the
  existing `Sync` scan logic per changed file.
- An edit produces a **new episode** with a new content hash (`ECOSYSTEM.md` C4), subject
  to the existing minimum-diff threshold so a keystroke-level save does not become version
  spam.

**Pull is the default; push is allowed where already shipped.** The blanket objection to
host push — that it puts a retry queue and an offline buffer in every host, past the
≤ 200-line budget, and risks silent loss while the daemon is down (the failure C1 exists to
prevent) — holds for a host that has no other durability. It does **not** hold for oxios's
`KnowledgeLens`, which pushes from a vault that oxios itself git-commits on every change
(oxios `src/kernel.rs:130-200`), so a missed push is recoverable by a later scan. Pull
degrades to "ingested late", which is correct behaviour; push plus pull is safe because
`insert_episode` deduplicates on `(space_id, content_hash)`
(`crates/oxibrain-store/src/ledger.rs:65-80`).

Rule: **each vault declares exactly one primary mechanism in config**, and the other is a
backstop rather than a second source of truth. oximemo's vault is pull (the brain owns
discovery); oxios's is push (the host already owns the change event).

Acceptance: save a file → a new episode within the debounce window; unchanged files are
never re-ingested; a daemon restart mid-scan produces zero duplicates.

---

## 6. Ingest provenance and CLI parity

### 6.1 Provenance on the ingest API — do this first

D-1 is the only unrepairable defect, so it leads. `ingest` and `remember` must accept and
persist provenance instead of hardcoding it:

- optional `source_kind` (`note` | `conversation` | `agent_trace` | `event` | `web_clip`)
  mapping to the corresponding `SourceRef` variant, default `note`;
- optional `trust` (`trusted` | `semi_trusted` | `untrusted`), default `semi_trusted` for
  anything arriving over the wire, `trusted` only for a locally-declared vault path.

Changing the default from today's implicit `Trusted` to `semi_trusted` is deliberate: a
remote MCP client should have to *claim* trust rather than receive it silently. This is an
additive schema change to the two tools' input schemas, so it does not break existing
clients — it changes what an omitted field means, which must be called out in the
`CONSUMPTION_CONTRACT` and the changelog.

Acceptance: a host can ingest an `agent_trace` at `semi_trusted` and a `web_clip` at
`untrusted`; a belief supported only by `untrusted` episodes reports higher uncertainty
(P10); `TrustTier` appears in the MCP server; and the parity/determinism suites stay green.

### 6.2 CLI parity

The console must never be the *only* way to do something. Add:

| Command | Emits |
|---|---|
| `oxibrain entity merge <a> <b>` | Declaration (Merge) |
| `oxibrain entity split <id>` | Declaration |
| `oxibrain entity alias <id> <name>` | Declaration |
| `oxibrain retract <statement-id>` | Declaration (Retract) |
| `oxibrain declare <predicate> <subject> <object>` | Declaration |
| `oxibrain predicate add <name> …` | registry write |

Division of labour: **the CLI is for automation and scripting; the console is for
judgement.** The merge queue's value is triage ergonomics — candidates side by side,
batch approve — not the write itself.

Acceptance: each command produces a Declaration episode, and the truth reprojection
determinism test still passes byte-identically after a sequence of them.

---

## 7. Invariants

These are enforceable. Violating one is a bug even if tests pass.

- **V1 — One owner per verb.** A second implementation of an owned verb is a bug.
- **V2 — The console never authors.** No editor, no file write, no vault management.
- **V3 — The console never generates prose.** It renders stored `Derived` episodes and
  projection rows. Triggering a batch operation (`reproject`, `reextract`, `extract`) is
  permitted; synthesising an answer is not. Testable: no console route calls a tool that
  performs synchronous inference for display.
- **V4 — Hosts never curate.** No host exposes `merge_entities`, `retract`, `declare`,
  `redact`, or predicate-registry writes.
- **V5 — Host brain reads are contextual.** Scoped to the note, session, or selection at
  hand. General browsing is the console's.
- **V6 — CLI parity.** Every console write verb has a CLI equivalent, so the product is
  complete with no browser.
- **V7 — The console is delivered.** Embedded in the binary and built by CI. An
  undelivered console is equivalent to no console, and its absence silently removes the
  only curation surface (D-2).
- **V8 — One store per authoring role, and the role is declared in the ledger.** The
  oximemo vault is human-authored; oxios's `<workspace>/knowledge` is agent-authored with a
  human review surface. Both may exist. What may not exist is a store whose episodes enter
  the ledger without their author, source kind, and trust — that is the difference between
  two legitimate substrates and two indistinguishable ones.
- **V10 — One primary ingest mechanism per vault.** Push or pull, declared in config. The
  other may act as a backstop; content-hash dedup makes that safe
  (`ledger.rs:65-80`), but two mechanisms both treated as primary means neither owns
  recovery.
- **V9 — One desktop application.** oximemo. The console is a browser app served by the
  daemon.

Existing invariants unaffected: `ARCHITECTURE.md` P1–P11 and `ECOSYSTEM.md` C1–C8 all
still hold. V4/V5 are the enforceable form of C6 (integration is a client dependency);
V8 is the enforceable form of C2 (one space, many sources — which requires the sources to
be distinguishable) and of C3 (files are edited by their owner).

---

## 8. Sequence

Each phase is independently shippable and independently valuable.

| Phase | Work | Acceptance |
|---|---|---|
| **P0** | This document + `ADR-008`; `ECOSYSTEM.md` → v2; `ARCHITECTURE.md` §1.3 third row becomes "console served by the daemon" with a version bump | docs contain no claim contradicted by source (D-2's stale `entity merge\|split\|alias` doc comment and D-3's "watch → episode" are corrected) |
| **P1** | **Ingest provenance (§6.1)** | `agent_trace` at `semi_trusted` and `web_clip` at `untrusted` round-trip; `TrustTier` reachable over MCP; uncertainty reflects trust |
| **P2** | CLI curation parity (§6.2) | six commands land; determinism test green after a merge/retract sequence |
| **P3** | Console delivery (§3.4) | `cargo install` → working console, no bun |
| **P4** | Console scope (§3.2–3.3): drop `/ask`, demote `/capture`, add space switcher, `/predicates`, `/failures`, `/operate` | zero hardcoded space literals; V3 test passes |
| **P5** | Daemon vault watcher (§5) + per-vault mechanism declaration (V10) | save → episode; no duplicates across restart or across push+pull overlap |
| **P6** | oxios alignment (§4.2): lens passes frontmatter provenance; `/brain` pinned to session scope; `brain curate` fixed or removed | no agent `Raw` note enters the ledger at human trust |
| **P7** | Unify design tokens from `project-oxi/.github/DESIGN.md` across console + oximemo | one token source; both apps build from it |

**P1 leads because it is the only unrepairable defect.** Episodes are append-only, so every
day the daemon runs, more rows are permanently stamped `Trusted` and `Note`. Nothing later
in this sequence can fix them; reprojection rebuilds the projection *from* those rows.

P2 before P3 deliberately: it closes the curation defect (D-2) without waiting on a
front-end pipeline, and it gives P3's console something to be a nicer front-end *for*.
P6 depends on P1 — the lens cannot declare provenance the API will not carry.

---

## 9. Rejected alternatives

**A — oximemo hosts everything; delete the console.** One desktop app, one design system,
one release pipeline; genuinely fewer moving parts for a solo developer, and it makes the
"oximemo + oxibrain only" combination the flagship. Rejected on one constraint:
`ARCHITECTURE.md` §1.5 requires that `cargo install oxibrain` alone be a complete second
brain. Under A, a standalone user gets no graph, no merge queue, no conflict triage — a
brain you cannot inspect without installing a memo app is not standalone. Secondary costs:
merge review needs entity pickers, statement rendering, and predicate semantics inside
oximemo (past the 200-line budget); oximemo's "no model, no prompt, no embedding" promise
blurs; and half its chrome darkens when the daemon stops.

**B — a shared JS component package (`@oxi/brain-views`) consumed by console, oxios, and
oximemo.** Would remove the one genuine triplication: three implementations of "render a
belief with its provenance". Deferred, not rejected on principle — it couples four
repositories in lockstep, and the views are still changing. Revisit once the console's
scope (§3.2) has been stable for two releases.

**C — host-push ingest as a blanket prohibition.** Rejected as stated; see §5. Push is
permitted where the host already owns durability for the pushed content.

**D — a second Tauri app for the console.** Costs a second signing, notarisation, and
auto-update pipeline for a surface a user opens occasionally. The daemon can serve a
browser app for free. Rejected.

**E — Leptos / Dioxus / egui / Maud+HTMX for the console.** See
`doc/adr/ADR-008-console-technology.md`.

**F — deleting oxios's KnowledgeBase editor** (v1.0's §4.2 recommendation). Rejected on
evidence: the vault is predominantly agent-written and agent-read, it is already ingested
into the daemon, and the human editor is the review surface for `needs_review` notes.
Deleting the write path would leave agents reading notes nobody can correct. See §1.3
and §4.2.
