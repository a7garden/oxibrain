# ADR-008 — Console technology: keep React + Vite, embed the bundle

> **Date:** 2026-08-19 · **Status:** Accepted
> **Context doc:** `doc/spec/ecosystem-v2-verb-ownership.md` §3
> **Supersedes nothing.** Amends `doc/ARCHITECTURE.md` §1.3: the third delivery shape is a
> *console served by the daemon*, not a desktop application.

## Context

The oxibrain console (`apps/brain-ui`) exists — eight routes, TanStack Router + Query,
`sigma` + `graphology` + `forceatlas2` for the force-directed graph — but it is not
delivered: `apps/brain-ui/.gitignore` ignores `dist/`, CI has no `bun` step, and
`crates/oxibrain-mcp/src/server.rs:1620-1656` reads static files from a `--ui-dir` the
user must build themselves.

Since oxibrain is a Rust-first project, the reasonable question arose: rewrite the console
in Rust, so the repository has one language and one toolchain. The console is
administrative — mostly tables, lists, and confirm dialogs — so a heavy front-end
framework looks like overkill.

## Decision

**Keep React 19 + Vite. Commit the built bundle, embed it with `include_dir!`, and gate it
in CI.** Reject Leptos/Dioxus, egui, and (for now) Maud + HTMX.

## Reasoning

### "Pure Rust" does not remove the build step

Leptos and Dioxus web targets require `wasm32-unknown-unknown`, `wasm-bindgen`, and
`trunk` or `dioxus-cli`. That **replaces** the bun/vite toolchain with a different
toolchain; it does not eliminate one. The stated benefit therefore does not materialise,
while the rewrite cost is fully paid.

### The rewrite buys nothing a user can see

Eight working routes would be rewritten to produce identical behaviour. The actual blocker
was never the language — it was delivery (`.gitignore`, no CI step, no embedding), which is
roughly fifteen lines of Rust plus one CI job.

### No Rust peer for the graph layout

`sigma` + `graphology-layout-forceatlas2` has no mature Rust/WASM equivalent. A Rust
rewrite means hand-writing canvas rendering and a force-directed layout, or wrapping
`sigma` through JS interop — which forfeits the "pure Rust" premise anyway.

Note this argument is *weakening by design*: `ecosystem-v2` §3.2 demotes the force graph to
a secondary toggle behind a ranked n-hop neighbour list. See "Revisit trigger".

### egui costs a second binary and the shared design tokens

`egui`/`eframe` is native, so the daemon cannot serve it: it becomes a second executable to
sign, notarise, and auto-update — the cost blueprint §9 D exists to avoid. It also cannot
share CSS custom properties with oximemo, and unifying
`tokens/primitives.css` + `semantic-dark.css` across the console and oximemo is a concrete
planned gain (blueprint P6). Korean text additionally requires bundling a variable font and
accepting weaker text layout.

### Maud + HTMX is the real pure-Rust candidate — and still loses today

Server-rendered `maud`/`askama` templates compiled into the binary via the derive/macro
path would be genuinely pure Rust, need no committed artifact, and suit a surface that is
90% tables and forms. Two blockers:

1. The eight existing views would be rewritten for no user-visible gain.
2. oxibrain's HTTP layer is a **hand-rolled raw TCP server**
   (`crates/oxibrain-mcp/src/server.rs:1712 write_http_response`). No `axum`, `tower-http`,
   `actix`, `warp`, `rust-embed`, or `include_dir` is a named dependency anywhere in the
   workspace (verified: zero matches across all `Cargo.toml`); `reqwest` is present solely
   as an HTTP *client* for `oxibrain-llm-http`. Server-side rendering with forms,
   redirects, and content negotiation on top of that transport means introducing `axum` and
   porting the shipped, tested MCP HTTP transport onto it.

Serving HTML from a second HTTP stack beside the hand-rolled one is worse than either
alternative.

## Consequences

### Accepted cost: a committed build artifact

`cargo install oxibrain` cannot run `bun`, so `apps/brain-ui/dist/` must be committed and
listed in the owning crate's `include`. Committed build artifacts drift and add review
noise. Mitigated structurally, not by discipline:

- CI runs `bun run build` then `git diff --exit-code apps/brain-ui/dist` — a bundle that
  does not match its source **fails the build**.
- CI asserts gzipped bundle size ≤ 400 KB.

### Accepted cost: two languages in the repository

Confined to `apps/brain-ui/` and one CI job. The Rust crates gain no JS dependency; the
boundary is a static directory.

### Rejected mitigation: a `ui` cargo feature, off by default

Would keep `dist/` out of the published crate by embedding only in CI-built release
binaries. Rejected because it makes `cargo install oxibrain` a second-class install with no
curation surface, which contradicts `ARCHITECTURE.md` §1.5.

## Revisit trigger

Migrate to **Maud + HTMX** — not Leptos, not egui — when either holds:

1. The force-directed graph is removed (blueprint §3.2 already demotes it), eliminating the
   `sigma`/`graphology` dependency and with it the strongest argument for a JS runtime; or
2. `axum` enters the workspace for another reason, making server-side rendering cheap.

At that point the console's content is tables, lists, and forms, and the pure-Rust option
becomes strictly better. Until then, this decision is "deliver what exists" rather than
"rewrite what works".
