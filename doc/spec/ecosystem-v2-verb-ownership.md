# Ecosystem v2 — Verb Ownership and the oxibrain Console

> **Version:** v1.0 · **Date:** 2026-08-19
> **Status:** Design blueprint. Canonical for *which surface owns which human verb*.
> On acceptance this supersedes `doc/ECOSYSTEM.md` v1.0 §3 and amends
> `doc/ARCHITECTURE.md` §1.3 (the "desktop app" delivery shape).
> **Companions:** `doc/adr/ADR-008-console-technology.md` (console tech choice),
> `doc/spec/brain-ui-v2.md` (current console implementation), `doc/ROADMAP.md`.
> **Method:** every claim about the current state below was verified against source
> at the cited path. Where the shipped code contradicts an existing doc, the code wins
> and the contradiction is called out.

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

**D-1 — oxios's KnowledgeBase vault is an orphan.** A human authors markdown at
`PUT /api/knowledge/file/{path}` → `knowledge.note_write` into an oxios-owned vault, and
**no code path ingests it into oxibrain**. Two authoring surfaces exist and one is a dead
pipe. This is worse than duplication: it teaches the user that writing does not reach
memory.

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
- The vault is the ecosystem's single human-authored text store.

### 4.2 oxios — two changes

1. **Delete the KnowledgeBase markdown editor** (`web/src/components/knowledge/markdown-editor.tsx`,
   `web/src/hooks/use-knowledge.ts` `useWriteFile`, `PUT /api/knowledge/file/{path}`,
   `knowledge.note_write`). It writes into a vault the brain never reads (D-1). Human
   authoring is oximemo's.
   *Considered and rejected:* keeping the editor but repointing it at the oximemo vault.
   Two editors over one store eventually diverge on wiki-link, tag, and template
   semantics, and the divergence is invisible until the connector mis-parses a file.
2. **Pin `/brain` to session context.** The four tabs stay read-only and stay scoped to
   "relevant to this session". General browsing is the console's.

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

*Considered and rejected:* host push (oximemo calls `ingest` after save). It puts a retry
queue and an offline buffer inside every host, which breaks the ≤ 200-line integration
budget and risks silent loss when the daemon is down — the exact failure C1 exists to
prevent. Pull-with-watch degrades to "ingested late", which is correct behaviour.

Acceptance: save a file → a new episode within the debounce window; unchanged files are
never re-ingested; a daemon restart mid-scan produces zero duplicates.

---

## 6. CLI parity

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
- **V8 — One human-authored text store.** The oximemo vault. A second one is an orphan
  by construction.
- **V9 — One desktop application.** oximemo. The console is a browser app served by the
  daemon.

Existing invariants unaffected: `ARCHITECTURE.md` P1–P11 and `ECOSYSTEM.md` C1–C8 all
still hold. V4/V5 are the enforceable form of C6 (integration is a client dependency);
V8 is the enforceable form of C3 (files are edited by their owner).

---

## 8. Sequence

Each phase is independently shippable and independently valuable.

| Phase | Work | Acceptance |
|---|---|---|
| **P0** | This document + `ADR-008`; `ECOSYSTEM.md` → v2; `ARCHITECTURE.md` §1.3 third row becomes "console served by the daemon" with a version bump | docs contain no claim contradicted by source (D-2's stale `entity merge|split|alias` doc comment and D-3's "watch → episode" are corrected) |
| **P1** | CLI curation parity (§6) | six commands land; determinism test green after a merge/retract sequence |
| **P2** | Console delivery (§3.4) | `cargo install` → working console, no bun |
| **P3** | Console scope (§3.2–3.3): drop `/ask`, demote `/capture`, add space switcher, `/predicates`, `/failures`, `/operate` | zero hardcoded space literals; V3 test passes |
| **P4** | Daemon vault watcher (§5) | save → episode; no duplicates across restart |
| **P5** | oxios cleanup (§4.2) | zero `note_write` paths in oxios; one authoring vault |
| **P6** | Unify design tokens from `project-oxi/.github/DESIGN.md` across console + oximemo | one token source; both apps build from it |

P1 before P2 deliberately: it closes the correctness defect (D-2) without waiting on a
front-end pipeline, and it gives P2's console something to be a nicer front-end *for*.

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

**C — host-push ingest.** See §5.

**D — a second Tauri app for the console.** Costs a second signing, notarisation, and
auto-update pipeline for a surface a user opens occasionally. The daemon can serve a
browser app for free. Rejected.

**E — Leptos / Dioxus / egui / Maud+HTMX for the console.** See
`doc/adr/ADR-008-console-technology.md`.
