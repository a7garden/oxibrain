# ADR-018 — Experience memory rides the episode model

Status: accepted — implemented 2026-10-16 (M11 shipped; `doc/ROADMAP.md` §7). Date: 2026-10-16.

## Context

LongMemEval-V2 (arXiv:2605.12493) reframes agent-memory evaluation around environment
experience — workflow knowledge, environment gotchas, premise awareness — and shows that
conversational-memory methods miss it on noisy trajectories. The industry response is to
add new memory kinds: Mem0 a "procedural" type, Letta editable memory blocks, AgentRunbook
file pools. oxibrain needs the capability without a second store: P1 forbids durable state
the ledger cannot replay, and the sixteen-tool cap forbids surfacing it as new operations.

## Decision

Trajectories are **Primary episodes** (`SourceRef::AgentTrace { agent }`, `Untrusted` by
default). Experience claims are **ordinary registry predicates** — `requires`,
`fails_when`, `premise_differs`, consuming M10's negative family. Ordered procedures exist
**only as Derived runbook episodes** — terminal, cached, uncertainty-bearing, never
re-extracted.

The load-bearing half of this decision: **the truth half stays atemporal propositions.**
A workflow step's identity is positional ("step 3 of deploy"), and the fold has no
positional semantics — no cardinality, invalidation, or supersession rule makes sense of
"step 3 became step 4". Order is a property of a *rendering*, so it lives in the derived
half as prose, where P10 already forces sources and uncertainty. What is propositionally
stable about a workflow — prerequisites, failure conditions, broken premises — is exactly
what becomes assertions.

## Consequences

- Workflow questions answer through the `Summaries`-layer runbooks plus member expansion,
  not through graph traversal of steps. Accepted: LME-V2's own data says notes plus raw
  evidence is the winning configuration (AgentRunbook-C beat every fixed-pipeline RAG
  design), and §12.4 already forbids summaries without sources.
- No new store, plane, memory type, or MCP tool. The registry bump is minor (no cache
  invalidation); the only migration is a backfill of legacy `agent_trace` rows.
- Cost concentrates where it should: extraction few-shots for noisy traces, and one
  consolidation mode.
- If the M11 exit gate shows (c) − (b) ≤ 0 on the gotcha/workflow categories with a strong
  extractor, the predicates remain useful registry data but runbook consolidation ships
  opt-in (ROADMAP §9 risk row).
