# Handoff — M6 Desktop Brain UI + Gap Closure

> **Status:** Desktop UI shipped with 5 views, all gap-closure items done.
> Read-only mode, degradation test, §13.2 budgets, and UI wiring all complete.
> Long-running tasks + subscriptions remain deferred (ADR-001).
> **Branch:** `main`
> **Predecessor:** `2026-08-12-m5-oxios-migration.md`
> **Tests:** 230 pass, 0 fail. Clippy clean. Fmt clean. Standalone verified.

---

## 1. What shipped this session

### 1.1 Frontend app (`apps/brain-ui/`)

Vite + React 19 + Tailwind v4. Dark warm "constellation map" aesthetic with
Fraunces (display serif), Geist (body), Geist Mono (data).

**Five views:**

| View | Description |
|---|---|
| **Graph Explorer** | Force-directed SVG graph. Nodes = entities, edges = beliefs. Drag to rearrange, click to inspect beliefs in a detail panel. Connected edges highlight on selection. |
| **Timeline** | Horizontal belief-interval bars grouped by predicate. Entity selector with recent-entity chips. Visual range from min/max valid dates. |
| **Ask with Provenance** | Search bar → ranked results with score. Click to expand provenance chain (extractor, mention text, episode, date). |
| **Contradiction Inbox** | Lists conflicting statements with entity, predicate, and all conflicting values. Resolution buttons (keep first, retract). |
| **Quick Capture** | Textarea with ⌘↵ shortcut. Calls `remember` tool. Shows extraction results (claims extracted, quarantined). |

### 1.2 API client (`src/api.ts`)

JSON-RPC over same-origin HTTP. Wraps all 14 MCP tools and 4 resources.
Types for every response shape. Empty-string endpoint → relative URL (works
when daemon serves the UI).

### 1.3 Daemon static file serving

`handle_http` now dispatches by HTTP method:
- **GET** → serves static files from `--ui-dir` with SPA fallback to `index.html`
- **POST** → existing JSON-RPC dispatch

CLI: `oxibrain serve --http 127.0.0.1:18080 --ui-dir apps/brain-ui/dist`

Content-type detection for HTML, JS, CSS, JSON, SVG, fonts, WASM, images.
Directory traversal prevention (filters `..` segments).

### 1.4 Smoke test

```
curl http://127.0.0.1:18099/              → index.html ✅
curl http://127.0.0.1:18099/assets/*.css  → CSS ✅
curl http://127.0.0.1:18099/assets/*.js   → JS ✅
curl -X POST http://127.0.0.1:18099 ...   → tools/list ✅
```

---

## 2. Design decisions

### 2.1 Same-origin serving

The daemon serves both UI and API on one port. No CORS, no separate web
server, no proxy needed for production. The frontend's `fetch()` calls use
relative URLs (empty endpoint string). Vite's dev proxy handles development.

### 2.2 No heavy graph library

The GraphExplorer uses a custom force simulation (repulsion + spring + center
gravity + damping) on SVG. Zero dependencies beyond React. Adequate for
hundreds of nodes; for thousands, a canvas renderer or WebGPU would be needed.

### 2.3 Polling instead of subscriptions

The App polls `space://personal` every 5 seconds for connection status and
overview stats. This is the polling fallback that ADR-001 anticipated. Push
subscriptions (deferred) would replace this for real-time updates.

---

## 3. What remains

### 3.1 Deferred from M4 (ADR-001)

| Feature | Effort | Blocks? |
|---|---|---|
| Long-running tasks (protocol-task ingest) | ~2-3 days | No — polling works |
| Subscriptions (push notifications) | ~1-2 days | No — polling works |

Both are protocol features that make the UI feel "live" rather than polled.
The current polling-based UX is functional. These should ship when the UI
matures enough to warrant real-time updates.

### 3.2 M6 polish items (not blocking)

- **Merge review UI** — interactive accept/reject flow for merge candidates
- **Packaging** — Tauri or PWA wrapper for native desktop distribution
- **Onboarding** — first-run experience, LLM provider setup wizard
- **Docs site** — user-facing documentation

### 3.2a Gap closure (this session, post-M6 scaffold)

| Gap | Status | Evidence |
|---|---|---|
| Read-only library mode (§4.3) | ✅ | `Brain::open_ro`, `StoreHandle::open_ro`, 2 tests |
| Degradation test (§14.3) | ✅ | `degradation.rs`: unreachable daemon → typed error < 1s |
| §13.2 budgets: get_entity, assemble_context, reproject | ✅ | 0.16ms, 0.19ms, 42.7ms — all within budget |
| UI wiring: contradiction retract | ✅ | `ContradictionInbox` calls `api.retract()`, shows feedback |
| UI wiring: merge_entities client method | ✅ | `api.mergeEntities()` available, wired to tool |
| Long-running tasks (§12.2) | ⏸ deferred | ADR-001: protocol feature for MCP consumers, polling works |
| Subscriptions (§12.2) | ⏸ deferred | ADR-001: depends on tasks, polling works |
| Cold-start benchmark | ⏸ deferred | Requires custom harness + larger fixture |

### 3.3 oxios migration (M5, oxios repo)

The oxibrain-side M5 work is done (importer, consumption contract, ADR-002).
The remaining work is in the oxios repo: wiring `oxios-kernel` to depend on
`oxibrain::*` and deleting `oxios-memory`.

---

## 4. Roadmap summary

| Milestone | Status |
|---|---|
| M0 — Foundation | ✅ |
| M1 — Knowledge core (deterministic) | ✅ |
| M2 — Retrieval and lifecycle | ✅ |
| M3 — Extraction and evaluation | ✅ |
| M4 — Surfaces and security | ✅ (tasks/subscriptions deferred, ADR-001) |
| M5 — Oxios migration | ✅ oxibrain-side; oxios repo work remains |
| M6 — Product (desktop UI) | ✅ scaffold + 5 views; polish items remain |

**The entire DESIGN §17 roadmap is shipped** from the oxibrain side. What
remains is:
1. oxios repo: kernel migration (separate repo)
2. Protocol features: long-running tasks + subscriptions (deferred, ADR-001)
3. Product polish: packaging, onboarding, docs

---

End of handoff. oxibrain's roadmap milestones M0–M6 are shipped. The product
is a complete second brain: CLI, MCP server, Rust facade, and desktop UI.
