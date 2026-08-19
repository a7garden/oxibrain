# Ecosystem v2 — Authorities, Events, and the Memory Kernel

> **Version:** v2.0 · **Date:** 2026-08-19
> **Status:** Proposed ecosystem blueprint. Replaces v1.1 after approval.
> **Scope:** Product boundaries and cross-repository contracts for oxibrain, oximemo,
> and oxios. Existing architecture documents are inputs, not constraints.
> **Companions:** `doc/ARCHITECTURE.md`, `doc/ECOSYSTEM.md`,
> `doc/adr/ADR-008-console-technology.md`.

---

## 0. Decision

The ecosystem has three products because it has three independent authorities:

| Product | Authority | Source of truth |
|---|---|---|
| **oximemo** | Human-authored knowledge | The user's `.md` / `.html` vault |
| **oxios** | Agent execution and work products | Sessions, traces, tasks, and artifacts |
| **oxibrain** | Memory interpretation | Append-only source events, with audited redaction, plus rebuildable projections |

oxibrain is a **memory kernel**, not a third daily application. Its stable products are
its daemon, Rust API, MCP/RPC surface, and complete CLI. It ships a small local management
console only for operations that require human judgement over oxibrain-native state.

The previous organizing rule — “each human verb has exactly one owning surface” — is
removed. Capture, query, and inspection may appear in several contexts. What must have one
owner is the underlying state and the semantics of each state transition.

> **One authority per source of truth. One implementation per mutation semantic. Many
> contextual projections are allowed.**

---

## 1. Why v1.1 was insufficient

v1.1 correctly identified an undelivered curation surface, missing CLI operations, absent
file watching, and flattened ingest provenance. It also correctly rejected a second Tauri
application and preserving oxios's editor while its KnowledgeBase still existed.

It made four deeper mistakes:

1. **It owned verbs at the UI layer.** CLI/GUI parity and contextual host views already
   require multiple affordances for the same verb. Duplicate affordances are harmless when
   they invoke one canonical application service.
2. **It accepted two long-term memory systems.** oxios writes and directly recalls its
   KnowledgeBase while also ingesting the same files into oxibrain. That is not merely two
   vaults with different authors; it is duplicate persistence, retrieval, curation, and
   authority.
3. **It treated content equality as event identity.** Deduplication by
   `(space_id, content_hash)` can erase independent provenance when two sources contain the
   same bytes. It also cannot represent a meaningful A → B → A revision sequence.
4. **It let producers assign trust.** A remote client may assert provenance, but it cannot
   decide the effective trust of its own assertions.

The final topology must fix these before expanding any user interface.

---

## 2. Target topology

```text
human documents                              agent execution
┌──────────────────────┐                    ┌──────────────────────┐
│ oximemo              │                    │ oxios                │
│ capture · author     │                    │ converse · act       │
│ .md/.html vault      │                    │ sessions · artifacts │
└──────────┬───────────┘                    └──────────┬───────────┘
           │ DocumentRevision                           │ Conversation
           │                                            │ AgentTrace
           │                                            │ ArtifactEvent
           └────────────────────┬───────────────────────┘
                                ▼
                    ┌────────────────────────┐
                    │ oxibrain memory kernel │
                    │                        │
                    │ immutable event ledger │
                    │ deterministic fold     │
                    │ rebuildable indexes    │
                    │ query/context services │
                    └────────────┬───────────┘
                                 │
             ┌───────────────────┼────────────────────┐
             ▼                   ▼                    ▼
       oximemo context      oxios context       CLI/admin console
       note-scoped          session-scoped      global repair only
```

The dependency direction is one-way:

- oximemo and oxios own primary source material.
- oxibrain observes source events and derives memory.
- hosts query memory for contextual assistance.
- oxibrain never edits host-owned source material.
- hosts never implement oxibrain's merge, retraction, predicate, or fold semantics.

A brain outage degrades memory assistance but never blocks capture, document editing, or
agent execution.

---

## 3. Authority model

### 3.1 Human document authority — oximemo

oximemo owns:

- instant capture and document authoring;
- vault file formats and file lifecycle;
- tags, categories, templates, links, and vault indexes;
- human review of a document before it becomes a durable human-authored source.

Its oxibrain integration is contextual and optional. It may show related entities,
contradictions, provenance, and prior material for the current note. It does not implement
memory merges, retractions, predicates, or global graph browsing.

The capture path remains independent of the brain process and its latency.

### 3.2 Agent execution and artifact authority — oxios

oxios owns:

- conversations, agent runs, traces, tasks, schedules, and execution audit;
- work products created by an agent for a task;
- review and promotion of work products before they become human documents;
- the chat and task interfaces through which humans direct agents.

A work product is not automatically long-term memory. oxios emits source events describing
what occurred; oxibrain decides what can be recalled or believed. If a human promotes an
artifact into their notebook, oximemo becomes the owner of the resulting document and a
new `DocumentRevision` source event records that fact.

oxios must not maintain a second semantic recall subsystem after migration. Session-local
working context and execution logs remain oxios responsibilities; cross-session semantic
memory belongs to oxibrain.

### 3.3 Memory interpretation authority — oxibrain

oxibrain owns:

- source registration and authenticated ingest;
- the append-only source-event ledger and its single audited redaction exception;
- extraction, resolution, temporal folding, uncertainty, and retrieval;
- declarations that correct memory interpretation;
- projection rebuilds, indexes, retention, redaction, and audit;
- query and context-assembly APIs used by every host.

It does not own a human document editor, an agent runtime, a conversation UI, or a general
purpose daily workspace.

### 3.4 Correction authority — declarations, not source edits

A human correction to memory interpretation is an append-only Declaration handled by
oxibrain: merge, split, alias, retract, declare, source-policy change, or predicate change.
Editing an oximemo document corrects the human source and creates a new source event;
editing an oxios artifact corrects the agent work product and creates a new source event.
Neither silently mutates existing memory rows. `redact` is separate: the sole destructive
privacy operation removes protected payload while appending an auditable redaction record.

---

## 4. Event contract

### 4.1 Canonical ingest envelope

Every ingest path converges on one application service and one event envelope:

```text
SubmittedIngestEvent
├── space_id
├── source_id
├── occurrence_id
├── source_kind
├── observed_at            # producer claim
├── payload
├── claims
└── predecessor_id?        # ordered sources only

Persisted ingress evidence additionally records:
├── principal_id           # authenticated transport identity
├── accepted_at            # server clock
├── content_hash
└── ingress_policy_id
```

`principal_id`, `accepted_at`, and `ingress_policy_id` are assigned by the server and are
never accepted from the payload. The server resolves `source_id` through its source
registry and verifies that the principal may append that `source_kind` to that source.

`claims` are producer assertions such as author, locator, title, quality, or review state.
They remain verbatim provenance. They are not trusted merely because they were supplied.

Initial source kinds:

- `DocumentRevision`
- `Conversation`
- `AgentTrace`
- `ArtifactEvent`
- `CalendarEvent`
- `WebClip`

Declarations are not a producer-selectable source kind. Privileged, typed oxibrain
commands append Declaration episodes through a separate application-service path.

A new source kind requires defined identity, ordering, trust policy, and replay semantics.
A string label alone is insufficient.

### 4.2 Event identity and idempotency

Event identity is:

```text
(space_id, source_id, occurrence_id)
```

`content_hash` verifies bytes and supports storage reuse. It is not an event identity.
Two independent sources containing identical text remain two independent episodes.

Push sources must provide a source-native immutable occurrence ID. Reusing an occurrence
ID with identical bytes is an idempotent retry; reusing it with different bytes is a hard
conflict and is audited.

For file connectors without a native revision ID, oxibrain derives:

```text
occurrence_id = H(source_id, normalized_locator, predecessor_id, content_hash)
```

The connector appends the episode and advances the source cursor in one transaction.
Therefore:

- a crash before commit recreates the same ID on retry;
- an unchanged file is ignored;
- A → B → A creates three events because the predecessor differs;
- mtime, wall-clock order, and process-local counters never define identity.

### 4.3 Push and pull

A registered source has exactly one primary ingestion mode:

- **pull:** oxibrain owns discovery and cursor advancement;
- **push:** the host owns change detection and supplies stable occurrence IDs.

A secondary mode may be configured as a recovery mechanism, but it must resolve to the
same `source_id` and occurrence-ID rules. Push and pull are not considered safe merely
because payload hashes match.

oximemo vaults default to pull. oxios conversations and traces are push. Generic imported
vaults are pull unless their owner implements the push contract.

---

## 5. Provenance and trust

### 5.1 Producers assert provenance; oxibrain evaluates trust

Clients may submit source claims. They may not submit an authoritative `TrustTier`.
Effective trust is derived from:

- authenticated principal;
- registered source;
- source kind;
- verified claims;
- the applicable source-policy declaration.

### 5.2 Trust policy is ledger state

Trust policy cannot be mutable external configuration because projection must remain a
pure, deterministic replay. Each new store begins with a content-derived kernel control
source. Capability-authorized commands append source registration, policy creation, and
policy change as Declaration events under that source before any external principal can
ingest into the registered source.

Each policy document has a content-derived `policy_id`. Canonical replay evaluates an
episode against the policy timeline for its source and effective interval. The server's
`accepted_at`, not the producer's `observed_at` claim, selects the default effective
policy. A retroactive policy correction is an explicit Declaration naming an effective
interval; it is never a silent config edit.

The predicate registry follows the same rule: predicate creation or semantic change is a
canonical Declaration, not mutable external configuration. Therefore the truth projection
is a pure function of:

```text
canonical source episodes + canonical policy declarations + canonical predicate declarations
```

Replaying the same ledger produces byte-identical truth state. Deliberately appending a
policy correction may change later projections; that change is explained by a ledger
episode and remains auditable.

The original authenticated principal, source claims, policy ID used at ingress, and
ingress assessment remain available as evidence even when a later policy declaration
changes effective trust.

### 5.3 Initial policy posture

- locally registered human vault: trusted document source;
- reviewed and promoted oxios artifact: semi-trusted until it becomes a human document;
- raw agent trace or conversation: semi-trusted evidence, never a human assertion;
- unreviewed agent scratch material: untrusted evidence and excluded from belief support
  until an explicit review event promotes it;
- web clip: untrusted unless promoted through an explicit review event;
- Declaration: authorized by capability and audited separately from source evidence.

These are bootstrap policy declarations, not hard-coded branches in the fold.

---

## 6. Product surfaces

### 6.1 oximemo

Daily human surface:

- global capture;
- authoring and document organization;
- vault-local search and graph;
- contextual, read-only brain panel;
- explicit promotion target for reviewed agent artifacts.

Its local graph represents document links. It is not the same projection as oxibrain's
entity relationships, and no attempt is made to merge those models.

### 6.2 oxios

Daily agent surface:

- chat, task, run, schedule, and artifact review;
- session-scoped memory context;
- explicit `remember` intent represented as an event, not a shadow markdown database;
- promotion of a reviewed artifact to oximemo through an explicit export/handoff.

The `/brain` view, if retained, is session-scoped diagnostic context. It is not a general
knowledge browser or curation surface.

### 6.3 oxibrain CLI

The CLI is the complete stable human and automation interface to the kernel:

- ingest, query, ask, search, explain, and context assembly;
- entity merge/split/alias;
- retract, declare, redact, and predicate management;
- source registration and policy declarations;
- reproject, reextract, doctor, backup, and restore;
- links that open a specific object or review item in the local console.

`ask` remains in the CLI as direct memory interrogation. It does not imply a second chat
product: there is no conversation workspace, task execution, or agent orchestration.

### 6.4 oxibrain management console

The console exists only where visual judgement materially improves correctness:

- merge candidate review;
- contradiction and retraction review;
- extraction-failure inspection and retry;
- provenance and source-policy inspection;
- entity/statement detail reached by a diagnostic deep link;
- space, source, health, model, reproject, backup, and restore operations.

It does not contain:

- a general ask/chat experience;
- capture or document authoring;
- a general knowledge homepage;
- an exploratory force graph;
- host task, note, or session management.

Definition:

> **The console is where memory is repaired, not where memory is lived.**

It is a browser bundle embedded in and served by the daemon. It is not a separate desktop
application or background service. React/Vite remains an acceptable implementation under
`ADR-008`; its scope, not its technology, changes here.

---

## 7. Removing oxios's duplicate memory

The current oxios KnowledgeBase cannot be deleted in place because agents write it, read
it into prompts, expose it in the web UI, git-commit it, curate it, and ingest it into
oxibrain. The migration removes the memory role before removing files or UI.

### Stage K1 — classify existing content

Each KnowledgeBase entry is classified as one of:

- **session evidence:** conversation/trace material that should be represented by source
  events;
- **work artifact:** a deliverable owned by oxios;
- **candidate human document:** content requiring explicit review and promotion to
  oximemo;
- **obsolete scratch material:** retained only for migration audit, then archived.

No automatic classifier may silently promote material to a trusted human document.

### Stage K2 — make oxibrain the cross-session recall path

oxios emits Conversation, AgentTrace, and ArtifactEvent records with stable source-native
IDs. Its per-turn context path reads oxibrain. The existing KnowledgeBase recall path is
measured in shadow mode, then disabled once parity and outage degradation are verified.

### Stage K3 — move artifact review to its owner

Agent work products remain in oxios and are reviewed there. A reviewed product may be
exported to oximemo, which mints the first DocumentRevision event. oxios does not edit the
resulting human document after handoff.

### Stage K4 — retire the KnowledgeBase product surface

Only after K2 and K3:

- stop new KnowledgeBase note creation;
- remove direct KnowledgeBase recall from agent prompts;
- archive existing vault content with a manifest and checksums;
- remove the KnowledgeBase editor and semantic markdown engine;
- retain generic workspace file tools only if agent execution independently needs them.

The target is not “zero markdown in oxios.” The target is “no second semantic memory
system in oxios.”

---

## 8. Cross-product contracts

### C1 — Primary work survives memory outage

oximemo captures and edits files; oxios runs agents and preserves session state. Memory
features degrade explicitly and recover when oxibrain returns.

### C2 — Source identity precedes content identity

Every episode is attributable to one registered source and one stable occurrence. Equal
bytes never erase independent provenance.

### C3 — Trust is evaluated, versioned, and replayable

Clients cannot grant themselves trust. Every policy change is ledger-explained and replay
deterministic.

### C4 — Sources are edited only by their owner

oxibrain returns annotations and declarations; it never rewrites host files or sessions.
Handoff creates a new target-owned artifact rather than shared write ownership.

### C5 — Mutation semantics have one implementation

All surfaces invoke oxibrain application services for merge, retract, declare, redact,
policy, predicate, and projection operations. Hosts may link to them but never reimplement
them.

### C6 — Contextual projections may repeat

The same belief may appear in oximemo, oxios, the CLI, and the management console. This is
not duplication if all views read the same canonical API and keep host-specific scope.

### C7 — No hidden dual recall

A host may retain session-local working context and lexical indexes for its own source.
It may not maintain a second cross-session semantic memory once oxibrain is authoritative.

### C8 — Handoffs are explicit events

Agent artifact → human document, web clip → reviewed source, and untrusted → trusted are
explicit user actions represented in the ledger. Moving data across authorities is never
a background side effect.

### C9 — Every console write has CLI parity

The management console improves judgement, not capability. Headless operation remains
complete.

### C10 — One desktop application

oximemo is the ecosystem's desktop application. oxios and oxibrain serve browser UIs from
their existing daemons; oxibrain's UI is only a management console.

---

## 9. Delivery order

The sequence follows irreversibility and dependency, not UI visibility.

| Phase | Ecosystem outcome | Exit condition |
|---|---|---|
| **P0 — Canon** | Adopt this blueprint; amend each repository's architecture docs and vocabulary | No canonical doc calls oxios KnowledgeBase a permanent memory authority or oxibrain a third daily app |
| **P1 — Event identity** | Introduce source registry, canonical `IngestEvent`, stable occurrence IDs, and conflict detection | Independent equal-content sources remain distinct; retries dedupe; A → B → A yields three events; full replay is byte-identical |
| **P2 — Trust policy** | Move trust assignment behind authenticated source policy declarations | Clients cannot self-grant trust; retroactive policy changes are ledger events; deterministic replay holds |
| **P3 — Host event adapters** | oximemo emits/presents DocumentRevision identity; oxios emits Conversation, AgentTrace, and ArtifactEvent | Both hosts survive daemon outage and resume without duplicates or provenance loss |
| **P4 — Curation parity** | Complete CLI merge/split/alias/retract/declare/predicate/source-policy operations | Every correction emits an auditable Declaration; reprojection remains deterministic |
| **P5 — Minimal console** | Deliver embedded repair/operations console and delete product-scope routes | `cargo install oxibrain-cli` can perform visual review without Node; no chat, capture, authoring, or general graph route |
| **P6 — Oxios recall cutover** | Make oxibrain the sole cross-session semantic recall path | Shadow comparison meets agreed recall quality; oxios runs without its KnowledgeBase recall path; outage behavior is explicit |
| **P7 — Artifact handoff** | Review artifacts in oxios and explicitly promote selected output to oximemo | A promoted artifact becomes an oximemo-owned document and produces one new DocumentRevision source event |
| **P8 — KnowledgeBase retirement** | Archive and remove oxios's semantic KnowledgeBase subsystem and editor | No agent prompt reads it; no new notes are written; archive manifest verifies all retained material |
| **P9 — Operational closure** | Watchers, backups, recovery, source health, and token/design cleanup | Crash/restart and migration suites pass across all three repositories |

P1 and P2 precede new ingest growth because identity and trust mistakes pollute an immutable
ledger. P6 precedes deletion because a working duplicate is removed only after its
replacement is measured. P8 is the final consequence of the authority model, not an
isolated UI cleanup.

---

## 10. System-level acceptance

The ecosystem is complete when all of the following are true:

1. A human captures and edits with oxibrain stopped; later synchronization emits each
   revision exactly once.
2. Two sources submit identical bytes and both remain independently attributable.
3. A file changes A → B → A and all three revisions survive replay.
4. A client attempts to mark its own event trusted and the request cannot elevate policy.
5. A source-policy correction changes effective trust through a Declaration and produces
   the same result on every replay.
6. An oxios agent runs with oxibrain stopped, preserves its session, and resumes memory
   integration without duplicate events.
7. An agent artifact is reviewed, promoted once to oximemo, and thereafter edited only by
   oximemo.
8. No oxios prompt path performs cross-session semantic recall from the retired
   KnowledgeBase.
9. Every visual memory correction is reproducible through the CLI and emits the same
   Declaration semantics.
10. No daily workflow requires opening the oxibrain console.

---

## 11. Rejected alternatives

### Independent oxibrain knowledge application

Rejected because it creates a third daily surface for search, graph browsing, capture, and
query. Those activities already have natural contexts in oximemo and oxios. Visual memory
repair remains justified; a general knowledge application does not.

### Completely headless oxibrain

Rejected because merges, contradictions, and provenance failures require visual comparison
for reliable human judgement. Forcing them into host applications leaks memory semantics;
forcing them into CLI-only workflows reduces correctness.

### Permanent dual vaults

Rejected because oxios's KnowledgeBase is not only an agent-authored file store. It also
performs cross-session recall and is ingested into oxibrain, creating two memory systems.
Artifacts may remain files, but semantic memory has one authority.

### Shared writable vault between oximemo and oxios

Rejected because it creates two owners for format, indexing, templates, links, review
state, and file lifecycle. Promotion is an explicit handoff, not shared write access.

### Client-supplied trust tier

Rejected because the producer cannot be the authority on its own trust. Producers submit
claims; authenticated server policy evaluates them.

### Content-hash event identity

Rejected because equal content can have different provenance and because reverting to
prior content is still a new historical occurrence.

### Shared cross-product UI package now

Deferred. Canonical APIs and deep links remove business-logic duplication first. A shared
rendering package is justified only after the minimal management console and contextual
host views stabilize independently.

---

## 12. Locked decisions and implementation-plan inputs

The following decisions are locked by this blueprint:

- oxibrain is a memory kernel with a minimal repair console;
- oximemo owns human documents;
- oxios owns agent execution and artifacts;
- oxios's semantic KnowledgeBase is transitional and will be retired after measured
  cutover;
- source/occurrence identity replaces content hash as the idempotency key;
- trust is server-evaluated from ledger-recorded policy declarations;
- host-to-host promotion is explicit and creates a new target-owned source event;
- mutation semantics live once in oxibrain and remain CLI-complete.

The implementation plan must statically trace the existing schemas, migrations, ledger
insert path, connector cursors, MCP tool schemas, oxibrain-client wire types, oximemo export
identity, oxios KnowledgeLens/recall/persistence paths, and console routes before assigning
cross-repository edits. It must include compatibility windows, migration fixtures,
rollback points, and cross-repository acceptance commands. No phase may remove an old read
or write path before its replacement has been observed in shadow mode where the blueprint
requires it.
