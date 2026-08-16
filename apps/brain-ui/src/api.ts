/// JSON-RPC client for the oxibrain MCP HTTP transport.
///
/// The daemon serves one JSON-RPC message per POST (stateless).
/// Default endpoint: http://127.0.0.1:18080

const DEFAULT_ENDPOINT =
  (import.meta.env.VITE_OXIBRAIN_URL as string) || "";
let nextId = 1;

interface JsonRpcResponse<T = unknown> {
  jsonrpc: "2.0";
  id: number;
  result?: T;
  error?: { code: number; message: string; data?: unknown };
}

/** Send a JSON-RPC request and return the result. */
export async function rpc<T = unknown>(
  method: string,
  params?: Record<string, unknown>,
  endpoint = DEFAULT_ENDPOINT,
): Promise<T> {
  const body = JSON.stringify({
    jsonrpc: "2.0",
    id: nextId++,
    method,
    params: params ?? {},
  });

  const res = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body,
  });

  if (!res.ok) {
    throw new Error(`HTTP ${res.status}: ${res.statusText}`);
  }

  const json: JsonRpcResponse<T> = await res.json();
  if (json.error) {
    throw new Error(json.error.message);
  }
  return json.result as T;
}

// ── MCP tool calls ──────────────────────────────────────────────────────

/** Call an MCP tool by name with arguments.
 *  Some tools return JSON (`search`, `recall`, `why`, `contradictions`, `stats`).
 *  Others return plain text (`brief`, `navigate`, `remember`, `merge_entities`,
 *  `retract`). Try JSON first; on parse failure return the raw text — callers
 *  typed `Promise<string>` will get the markdown / confirmation as-is. */
export async function callTool<T = unknown>(
  name: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const result = await rpc<{ content: Array<{ type: string; text: string }> }>(
    "tools/call",
    { name, arguments: args },
  );
  const text = result?.content?.[0]?.text;
  if (text === undefined) return result as unknown as T;
  try {
    return JSON.parse(text) as T;
  } catch {
    return text as unknown as T;
  }
}

// ── MCP resource reads ──────────────────────────────────────────────────

/** Read an MCP resource by URI. */
export async function readResource<T = unknown>(uri: string): Promise<T> {
  const result = await rpc<{
    contents: Array<{ uri: string; text: string }>;
  }>("resources/read", { uri });
  if (result?.contents?.[0]?.text) {
    return JSON.parse(result.contents[0].text) as T;
  }
  return result as unknown as T;
}

// ── Convenience wrappers ────────────────────────────────────────────────
//
// The JSON keys mirror the contract tests in
// crates/oxibrain-mcp/src/server.rs (tasks 2–4 of
// doc/plans/2026-08-16-brain-ui-v2.md).

export const api = {
  search: (query: string, space = "personal") =>
    callTool<SearchResult[]>("search", { query, space }),

  recall: (query: string, space = "personal") =>
    callTool<RecallResult>("recall", { query, space }),

  brief: (entityId: string, space = "personal") =>
    callTool<string>("brief", { entity_id: entityId, space }),

  navigate: (from: string, link: string, space = "personal") =>
    callTool<string>("navigate", { from, link, space }),

  traverse: (start: string[], space = "personal", depth = 2) =>
    callTool<TraversalResult>("traverse", {
      start,
      space,
      depth,
      max_nodes: 256,
    }),

  why: (statementId: string, space = "personal") =>
    callTool<ExplainBlock>("why", { statement_id: statementId, space }),

  contradictionDetails: (space = "personal") =>
    callTool<ContradictionDetail[]>("contradictions", { space }),

  remember: (content: string, space = "personal") =>
    callTool<string>("remember", { content, space }),

  listMerges: (space = "personal") =>
    callTool<MergeRecord[]>("review_merges", { space }),

  mergeEntities: (
    loserSurface: string,
    loserType: string,
    winnerSurface: string,
    winnerType: string,
    space = "personal",
  ) =>
    callTool<string>("merge_entities", {
      loser: { surface: loserSurface, type: loserType },
      winner: { surface: winnerSurface, type: winnerType },
      space,
    }),

  retract: (
    subjectSurface: string,
    subjectType: string,
    predicate: string,
    objectKind: string,
    objectValue: string,
    episodeId: string,
    space = "personal",
  ) =>
    callTool<string>("retract", {
      subject: { surface: subjectSurface, type: subjectType },
      predicate,
      object: { kind: objectKind, value: objectValue },
      episode: episodeId,
      space,
    }),

  /** Statement-first retract: the conflicts inbox holds statement ids, not
   *  resolvable surfaces or entity types — the server rebuilds the
   *  Declaration from the stored statement. Retraction denies ALL assertions
   *  of the statement and emits a Declaration episode. */
  retractStatement: (statementId: string, space = "personal") =>
    callTool<string>("retract", { statement_id: statementId, space }),
  spaceOverview: (space = "personal") =>
    readResource<SpaceOverview>(`space://${space}`),

  graphSnapshot: (entity: string, depth = 2, space = "personal") =>
    readResource<TraversalResult>(
      `graph://${entity}?depth=${depth}&space=${space}`,
    ),

  timeline: (entityId: string, space = "personal") =>
    readResource<TimelineEntry[]>(`timeline://${entityId}?space=${space}`),

  beliefs: (entityId: string, space = "personal") =>
    readResource<Belief[]>(`entity://${entityId}?space=${space}`),
};

// ── Types ───────────────────────────────────────────────────────────────

export interface EntityCard {
  id: string;
  surface: string;
  type: string;
}

export interface SpaceOverview {
  space: string;
  space_id: string;
  entity_count: number;
  episode_count: number;
  contradiction_count: number;
  recent_entities: EntityCard[];
}

export interface SearchResult {
  entity_id: string;
  entity_surface: string;
  entity_type: string;
  score: number;
  snippet: string;
}

export interface RecallResult {
  context: string;
  entities: string[];
}

/** Mirrors `oxibrain_core::knowledge::Belief` (struct wins; see brief). */
export interface Belief {
  statement: string;
  valid_from: number;
  valid_to: number;
  support: BeliefSupport;
  confidence: number;
  status: "active" | "superseded" | "contradicted" | "retracted";
}

export interface BeliefSupport {
  affirm_count: number;
  deny_count: number;
  distinct_episodes: number;
  /** Serde serializes tuples as JSON arrays: ["trusted", 2] etc. */
  trust_weights: Array<["trusted" | "semi_trusted" | "untrusted", number]>;
}

/** Mirrors `oxibrain_core::retrieval::TraversalNode`. */
export interface GraphNode {
  entity: string;
  depth: number;
  salience: number;
}

/** Mirrors `oxibrain_core::retrieval::TraversalEdge`. */
export interface GraphEdge {
  from: string;
  to: string;
  predicate: string;
  statement_id: string;
  depth: number;
}

/** Mirrors `oxibrain_core::retrieval::TraversalResult`. */
export interface TraversalResult {
  nodes: GraphNode[];
  edges: GraphEdge[];
  truncated: boolean;
}

/** Mirrors `oxibrain_store::query::ContradictionDetail`. */
export interface ContradictionDetail {
  statement_id: string;
  subject_id: string;
  subject_surface: string;
  subject_type: string;
  predicate: string;
  object_kind: "entity" | "literal";
  object_value: string;
  affirm_episodes: string[];
  deny_episodes: string[];
}

export interface AssertionDetail {
  assertion_id: string;
  episode_id: string;
  extractor: string | null;
  polarity: string;
  confidence: number;
  recorded_at: number;
}

/** Mirrors `oxibrain_core::knowledge::Statement` projection used by `why`. */
export interface ExplainBlock {
  statement: {
    id: string;
    space: string;
    subject: string;
    predicate: string;
    object: unknown;
  };
  status: string;
  assertions: AssertionDetail[];
  confidence_breakdown: {
    raw_confidence: number;
    support_count: number;
    contradiction_count: number;
  };
}

/** Mirrors `oxibrain_core::knowledge::EntityMerge`. */
export interface MergeRecord {
  id: string;
  loser: string;
  winner: string;
  /** Tagged enum from `MergeDecision` (`{kind, data?}`). */
  decided_by: { kind: "rule" | "user" | "import"; data?: unknown };
  provenance: string;
  evidence: string[];
  decided_at: number;
  undone_at: number | null;
}

/** Mirrors `oxibrain_store::timeline::TimelineEntry`. */
export interface TimelineEntry {
  statement_id: string;
  predicate: string;
  object_repr: string;
  object_entity: string | null;
  valid_from: number;
  valid_to: number;
  status: string;
  recorded_at: number;
}

// ── Pending-deletion shims ─────────────────────────────────────────────
//
// The pre-router views (GraphExplorer, ContradictionInbox) still reference
// `GraphNode`/`GraphEdge`/`Contradiction` shapes from a prior contract.
// Tasks 8–11 own their rewrites and will delete the views. Until then,
// these aliases keep the dead files typechecking under the new server-
// accurate types. New code MUST NOT use them.

export interface LegacyGraphNode {
  id: string;
  surface: string;
  entity_type: string;
}

export interface LegacyGraphEdge {
  from: string;
  to: string;
  predicate: string;
}

export interface LegacyContradiction {
  statement_id: string;
  entity_surface: string;
  predicate: string;
  conflicting_values: string[];
}