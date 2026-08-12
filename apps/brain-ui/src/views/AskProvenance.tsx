import { useState, useCallback } from "react";
import {
  api,
  type SearchResult,
  type ExplainBlock,
} from "../api";

export function AskProvenance() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [provenance, setProvenance] = useState<ExplainBlock | null>(null);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);

  const search = useCallback(async () => {
    if (!query.trim()) return;
    setLoading(true);
    setSearched(true);
    setExpanded(null);
    setProvenance(null);
    try {
      const r = await api.search(query.trim());
      setResults(r);
    } catch {
      setResults([]);
    }
    setLoading(false);
  }, [query]);

  const toggleProvenance = useCallback(
    async (entityId: string) => {
      if (expanded === entityId) {
        setExpanded(null);
        setProvenance(null);
        return;
      }
      setExpanded(entityId);
      setProvenance(null);
      try {
        const beliefs = await api.getEntity(entityId);
        if (beliefs.beliefs[0]) {
          const why = await api.why(
            `${entityId}:${beliefs.beliefs[0].predicate}`,
          );
          setProvenance(why);
        }
      } catch {
        // no provenance available
      }
    },
    [expanded],
  );

  return (
    <div className="flex h-full flex-col p-8">
      {/* Search bar */}
      <div className="mb-6 flex gap-2">
        <div className="relative flex-1">
          <span className="pointer-events-none absolute left-4 top-1/2 -translate-y-1/2 text-lg text-text-faint">
            ⌕
          </span>
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && search()}
            placeholder="Ask anything…"
            autoFocus
            className="w-full rounded-xl border border-line bg-surface py-3 pl-12 pr-4 text-text placeholder:text-text-faint focus:border-amber focus:outline-none"
          />
        </div>
        <button
          onClick={search}
          disabled={loading}
          className="rounded-xl bg-amber px-6 py-3 font-medium text-ink transition-opacity hover:opacity-90 disabled:opacity-50"
        >
          {loading ? "…" : "Ask"}
        </button>
      </div>

      {/* Results */}
      <div className="flex-1 overflow-auto">
        {searched && !loading && results.length === 0 && (
          <div className="flex h-full items-center justify-center">
            <p className="font-display text-lg text-text-faint">
              No results. Try a different query.
            </p>
          </div>
        )}

        {results.length > 0 && (
          <div className="space-y-3 stagger">
            {results.map((r, i) => (
              <div key={i}>
                <button
                  onClick={() => toggleProvenance(r.entity_id)}
                  className={`w-full rounded-xl border p-4 text-left transition-colors ${
                    expanded === r.entity_id
                      ? "border-amber bg-surface-2"
                      : "border-line bg-surface hover:border-line-bright"
                  }`}
                >
                  <div className="flex items-start justify-between gap-4">
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="font-display text-base text-text">
                          {r.entity_surface}
                        </span>
                        <span className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-xs text-text-faint">
                          {r.entity_type}
                        </span>
                      </div>
                      <p className="mt-1 truncate text-sm text-text-dim">
                        {r.snippet}
                      </p>
                    </div>
                    <div className="shrink-0 text-right">
                      <div className="font-mono text-sm text-amber">
                        {(r.score * 100).toFixed(0)}%
                      </div>
                    </div>
                  </div>
                </button>

                {/* Provenance expansion */}
                {expanded === r.entity_id && (
                  <div className="ml-4 mt-2 border-l border-amber-dim pl-4 animate-fade-in">
                    <h4 className="mb-2 font-mono text-xs uppercase tracking-wider text-text-faint">
                      Provenance
                    </h4>
                    {provenance ? (
                      <div className="space-y-2">
                        {provenance.assertions.map((a, ai) => (
                          <div
                            key={ai}
                            className="rounded-lg bg-surface/60 p-3"
                          >
                            <div className="flex items-center gap-2 font-mono text-xs text-text-faint">
                              <span className="text-amber-dim">
                                {a.extractor_id}
                              </span>
                              <span>·</span>
                              <span>{a.valid_from.slice(0, 10)}</span>
                            </div>
                            <p className="mt-1 text-sm italic text-text-dim">
                              "{a.mention_text}"
                            </p>
                            <div className="mt-1 font-mono text-xs text-text-faint">
                              episode: {a.episode_id.slice(0, 16)}…
                            </div>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <p className="font-mono text-xs text-text-faint">
                        Loading provenance…
                      </p>
                    )}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
