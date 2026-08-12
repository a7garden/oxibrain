# Phase M4 Handoff — Sampling LlmPort, Daemon Lifecycle, CLI Flattening → M5

> **Status:** All three remaining M4 items shipped. Sampling LlmPort is live:
> the session loop is bidirectional, the server can ask the client's model to
> extract, and a standalone user with Claude Desktop gets extraction without an
> API key. All gates green.
> **Branch:** `main` (squash commit `faa1477`)
> **Predecessor:** M4 client/http/eval handoff (`docs/superpowers/handoffs/2026-08-12-m4-client-http-eval.md`)
> **Tests:** 202 pass, 0 fail (+6 since last handoff). Clippy clean (`-D warnings`). Fmt clean. Standalone verified.

---

## 1. What this session shipped

The three remaining items from the prior handoff §5 are done.

| Item (prior handoff §5) | Status |
|---|---|
| **Sampling LlmPort (§12.3)** | ✅ Bidirectional session loop + `SamplingLlmPort` + `Sample` cap gating + realtime extraction |
| **Daemon lifecycle** | ✅ PID file (`--daemon`), graceful shutdown (SIGINT/SIGTERM), clear lock diagnostic |
| **Nested subcommand flattening** | ✅ `token issue\|list\|revoke`, `predicate list`, `entity show` |
| Session registry (batch sampling) | ⬜ Deferred — realtime-only per §12.3; batch routing is future work |
| Sampling audit logging | ⬜ Capability check exists; per-space audit not yet wired |

---

## 2. Architecture: what was built

### Bidirectional session loop (`oxibrain-mcp::server`)

The session loop was unidirectional (read line → handle → write response).
Sampling requires the server to send `sampling/createMessage` **to the client**
and await the response on the same stream. The new architecture:

```
                ┌─── write task ──── drain outbound channel → write to stream
session_loop ───┤
                └─── read loop ────── read lines from stream
                                     ├─ response (no method, matching id) → resolve pending oneshot
                                     └─ request (has method) → spawn dispatch task → outbound channel
```

- **Write task** owns the writer, drains an `mpsc::UnboundedReceiver<Value>`.
  Every message — client-request responses AND server-initiated requests —
  goes through it.
- **Read loop** owns the reader. For each line: if it's a JSON-RPC *response*
  (no `method`, has matching `id`), resolve the pending `oneshot`. Otherwise
  it's a client request — **spawned as a task** so the read loop stays free to
  receive sampling responses.
- **`SessionHandle`** (`oxibrain-mcp::sampling`) — outbound sender + pending map
  + id counter. Created once per session, cloned into each dispatch task.

**Critical shutdown fix:** `drop(session)` before `write_task.await`. The
`SessionHandle` holds an `outbound_tx` clone; without dropping it, the write
task's `recv()` hangs forever on client disconnect. A 5s timeout on the join
handles in-flight dispatch tasks (max 120s sampling timeout). Regression test
awaits the JoinHandle (not `_task`) to prove the session returns `Ok(())`.

### `SamplingLlmPort` (`oxibrain-mcp::sampling`)

An `LlmPort` that delegates `complete()` to the MCP client:

```
LlmRequest → {system + prompt} → sampling/createMessage → client's model
                                                            ↓
LlmResponse ← result.content.text ← client response ←──────┘
```

- 120s timeout. Client disconnect / refusal → retryable `BrainError::Provider`
  (§12.3: "ordinary outcome, not an error").
- Maps `LlmRequest.system` + `LlmRequest.prompt` → single user message.

### `Sample` capability gating

- **Trusted local channel** (scope == None, e.g. stdio with Claude Desktop):
  sampling available by default — this is the primary use case.
- **Authenticated session** (scope == Some): requires `Capability::Sample` in
  the token (§12.3: "off by default, granted per token and per space").

### Realtime extraction via sampling

The `ingest` MCP tool gained `extract: true`. When set:
1. Episode is created (`ingest_note`).
2. If sampling is available, a `SamplingLlmPort` is constructed from the
   session handle.
3. `Brain::extract_one_with(space, episode_id, config, llm)` runs extraction
   with the client's model (new method — takes an explicit `LlmPort`).
4. Result includes extraction summary (`N extracted, M quarantined`).

If sampling is unavailable, returns a skip message — the episode is still
ingested.

### Daemon lifecycle (`oxibrain-mcp::daemon`)

- **`PidFile`** — RAII guard: writes `<dir>/.oxibrain.pid` on creation, removes
  on drop (only if it still contains our PID). Stale files are overwritten on
  next start; the advisory lock is the real single-writer guard.
- **`shutdown_signal()`** — waits for SIGINT (Ctrl+C) or SIGTERM (what launchd
  sends). All socket/HTTP accept loops `select!` on it and break gracefully.
- **`--daemon` flag** — writes the PID file. Does NOT fork (§15: backgrounding
  is launchd's job).
- **Lock diagnostic** — when `Brain::open` fails with `Locked`, the CLI prints a
  clear message pointing to the existing daemon.

### CLI nested subcommands (DESIGN §12.4)

| Before (flat) | After (nested) |
|---|---|
| `oxibrain token-issue` | `oxibrain token issue` |
| `oxibrain token-list` | `oxibrain token list` |
| `oxibrain token-revoke` | `oxibrain token revoke` |
| `oxibrain predicate-list` | `oxibrain predicate list` |
| `oxibrain entity-show` | `oxibrain entity show` |

---

## 3. Verification (this session)

- **Sampling unit tests** (2): request/response mapping, client error is
  retryable. Both deliver responses through the pending map (simulating the
  read loop).
- **Sampling round-trip integration test**: client sends `ingest` with
  `extract:true` → server sends `sampling/createMessage` → client responds with
  `{"claims":[]}` → extraction completes → `Ingested + extracted` result. Full
  bidirectional protocol over a real duplex stream.
- **Disconnect regression test**: awaits the `run_session` JoinHandle (not
  `_task`) and asserts `Ok(())` within 3s after client disconnect. Would hang
  without the `drop(session)` fix.
- **Daemon PID file**: start `serve --daemon --socket` → PID file exists with
  correct PID → SIGTERM → PID file removed.
- **Lock diagnostic**: second `serve` on same dir → clear "store is locked"
  message.
- **CLI nested help**: `token --help`, `predicate --help`, `entity --help` all
  show nested subcommands.
- **Eval suite**: 5/5 triples, precision 1.000, recall 1.000, fabricated rate
  0.000. All §14.2 gates passed.
- **Full suite**: 202 pass, 0 fail (+6).
- **clippy** `--all-targets --all-features -D warnings`: clean.
- **fmt**: clean.
- **Standalone**: `oxibrain`, `oxibrain-mcp`, `oxibrain-client` cargo trees
  contain no `oxios-`/`oxicode-` crates; `--no-default-features --features
  http-llm` builds.

---

## 4. Test inventory

| Crate | Tests | Delta |
|---|---|---|
| oxibrain-ports | 4 | — |
| oxibrain-core | 70 | — |
| oxibrain-store (lib) | 22 | — |
| oxibrain-store (integration) | 10 | — |
| oxibrain (facade) | 8 | — |
| oxibrain-cli | 1 | — |
| oxibrain-llm-http | 3 | — |
| oxibrain-mcp | 30 | +4 (sampling ×2, round-trip ×1, disconnect ×1) |
| oxibrain-client | 7 | — |
| oxibrain-index | 5 | — |
| (other integration) | 42 | +2 (daemon PID ×2) |
| **Total** | **202** | **+6** |

---

## 5. Remaining work

1. **Session registry for batch sampling.** The current sampling path is
   realtime-only: extraction runs synchronously inside the request handler with
   the session's `SamplingLlmPort` in scope. Batch/nightly extraction (the job
   queue) would need a registry mapping `session_hint` → live session handle so
   the worker can route sampling requests. §12.3 explicitly scopes sampling to
   `realtime`; batch routing is a follow-on.

2. **Sampling audit logging.** The `Sample` capability is checked
   (`sampling_available()`), but per-space audit entries for sampling
   authorization aren't written yet. The audit infrastructure exists (M4
   security core); wiring it is straightforward.

3. **MCP client-capability advertisement.** The server doesn't check whether
   the client advertised sampling support during `initialize`. If the client
   doesn't support it, the sampling request fails gracefully (retryable error),
   but the server could skip the attempt entirely.

4. **M5 — oxios migration.** This was M4's terminal milestone. M5 is the next
   major phase.

---

## 6. Key decisions

- **D-sampling-1 — All requests dispatched in spawned tasks.** Uniform
  architecture: every client request spawns a dispatch task, regardless of
  whether it needs sampling. Keeps the read loop free to receive sampling
  responses at all times. JSON-RPC 2.0 matches responses by id, so
  out-of-order is fine. The per-request task overhead is negligible for an
  LLM-backed tool.

- **D-sampling-2 — Trusted stdio gets sampling implicitly.** A standalone user
  with Claude Desktop (stdio, no token) is the primary sampling use case.
  Requiring a `Sample`-capable token for the trusted local channel would defeat
  the purpose. Authenticated sessions (socket) require explicit `Sample` cap.

- **D-sampling-3 — `drop(session)` + 5s timeout on write_task.** The
  `SessionHandle` owns an `outbound_tx` clone. Without dropping it, the write
  task hangs forever on disconnect. The 5s timeout handles in-flight dispatch
  tasks that hold their own clones (max 120s sampling timeout); those tasks
  detach and clean up when they complete.

- **D-daemon-1 — PID file is informational, not a guard.** The advisory lock in
  `oxibrain-store` is the real single-writer enforcement (P8). The PID file
  exists so external supervisors (launchd) and monitoring tools can find the
  process. No liveness check — the lock prevents two daemons, and stale PID
  files are overwritten on next start.

---

## 7. Workspace changes

New files:
- `crates/oxibrain-mcp/src/daemon.rs` — `PidFile`, `shutdown_signal()`.
- `crates/oxibrain-mcp/src/sampling.rs` — `SessionHandle`, `SamplingLlmPort`.

New MCP exports: `PidFile`, `shutdown_signal` (from `daemon`).

New Brain method: `extract_one_with(space, episode_id, config, llm: Arc<dyn
LlmPort>)` — extraction with an explicit provider.

New CLI: `serve --daemon` flag; nested `token`/`predicate`/`entity` groups.

New MCP Cargo deps: `async-trait`, `tokio` `signal` feature.

---

End of handoff. M4 is feature-complete: the daemon topology ships with
bidirectional sampling, graceful lifecycle, and the canonical CLI surface.
Start M5 (oxios migration) from here.
