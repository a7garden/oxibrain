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

/** Call an MCP tool by name with arguments. */
export async function callTool<T = unknown>(
  name: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const result = await rpc<{ content: Array<{ type: string; text: string }> }>(
    "tools/call",
    { name, arguments: args },
  );
  // MCP tools return content blocks. Extract text.
  if (result?.content?.[0]?.text) {
    return JSON.parse(result.content[0].text) as T;
  }
  return result as unknown as T;
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

export const api = {
  search: (query: string, space = "personal") =>
    callTool<SearchResult[]>("search", { query, space }),

  recall: (query: string, space = "personal") =>
    callTool<RecallResult>("recall", { query, space }),

  getEntity: (entityId: string, space = "personal") =>
    callTool<EntityBeliefs>("get_entity", { entity_id: entityId, space }),

  traverse: (startEntities: string[], space = "personal", depth = 2) =>
    callTool<TraversalResult>("traverse", {
      start_entities: startEntities,
      space,
      depth,
    }),

  timeline: (entityId: string, space = "personal") =>
    callTool<TimelineEntry[]>("timeline", { entity_id: entityId, space }),

  contradictions: (space = "personal") =>
    callTool<Contradiction[]>("contradictions", { space }),

  why: (statementId: string, space = "personal") =>
    callTool<ExplainBlock>("why", { statement_id: statementId, space }),

  remember: (content: string, space = "personal") =>
    callTool<RememberResult>("remember", { content, space }),

  listMerges: (space = "personal") =>
    callTool<MergeRecord[]>("review_merges", { space }),

  spaceOverview: (space = "personal") =>
    readResource<SpaceOverview>(`space://${space}`),

  graphSnapshot: (entity: string, depth = 2, space = "personal") =>
    readResource<TraversalResult>(
      `graph://${entity}?depth=${depth}&space=${space}`,
    ),
};

// ── Types ───────────────────────────────────────────────────────────────

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

export interface Belief {
  predicate: string;
  object_surface: string;
  valid_from: string;
  valid_to: string | null;
  confidence: number;
  source_episode_id: string;
}

export interface EntityBeliefs {
  entity_id: string;
  entity_surface: string;
  entity_type: string;
  beliefs: Belief[];
}

export interface GraphNode {
  id: string;
  surface: string;
  entity_type: string;
}

export interface GraphEdge {
  from: string;
  to: string;
  predicate: string;
}

export interface TraversalResult {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface TimelineEntry {
  predicate: string;
  object_surface: string;
  valid_from: string;
  valid_to: string | null;
  confidence: number;
  episode_id: string;
}

export interface Contradiction {
  statement_id: string;
  entity_surface: string;
  predicate: string;
  conflicting_values: string[];
}

export interface ExplainBlock {
  statement_id: string;
  assertions: Array<{
    episode_id: string;
    extractor_id: string;
    mention_text: string;
    valid_from: string;
  }>;
}

export interface RememberResult {
  episode_id: string;
  extracted: number;
  quarantined: number;
  note: string;
}

export interface MergeRecord {
  id: string;
  canonical_id: string;
  merged_id: string;
  created_at: string;
}

export interface SpaceOverview {
  space: string;
  entity_count: number;
  episode_count: number;
  contradictions: number;
  recent_entities: Array<{
    id: string;
    surface: string;
    entity_type: string;
  }>;
}
