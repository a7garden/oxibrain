import type {
  Belief,
  ContradictionDetail,
  ExplainBlock,
  MergeRecord,
  SearchResult,
  SpaceOverview,
  TimelineEntry,
  TraversalResult,
} from "./api";
import { api } from "./api";

/** Query-key factory. All keys are tuples/const arrays so partial
 *  invalidation in mutations stays precise. */
export const qk = {
  space: ["space"] as const,
  search: (q: string) => ["search", q] as const,
  brief: (id: string) => ["brief", id] as const,
  timeline: (id: string) => ["timeline", id] as const,
  beliefs: (id: string) => ["beliefs", id] as const,
  graph: (id: string) => ["graph", id] as const,
  contradictions: ["contradictions"] as const,
  merges: ["merges"] as const,
  why: (sid: string) => ["why", sid] as const,
};

/** Thin fetcher wrappers — used as `queryFn` in `useQuery`. They keep the
 *  caching key contract flat: each fetcher returns the same shape as its
 *  key. */
export const fetchers = {
  space: (): Promise<SpaceOverview> => api.spaceOverview(),
  search: (q: string): Promise<SearchResult[]> => api.search(q),
  brief: (id: string): Promise<string> => api.brief(id),
  timeline: (id: string): Promise<TimelineEntry[]> => api.timeline(id),
  beliefs: (id: string): Promise<Belief[]> => api.beliefs(id),
  graph: (id: string): Promise<TraversalResult> => api.graphSnapshot(id),
  contradictions: (): Promise<ContradictionDetail[]> =>
    api.contradictionDetails(),
  merges: (): Promise<MergeRecord[]> => api.listMerges(),
  why: (sid: string): Promise<ExplainBlock> => api.why(sid),
};

/** Invalidate everything a write tool may have changed. */
export const invalidateAll = {
  space: qk.space,
  contradictions: qk.contradictions,
  merges: qk.merges,
};