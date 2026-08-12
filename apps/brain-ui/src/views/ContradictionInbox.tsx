import { useState, useEffect } from "react";
import { api, type Contradiction } from "../api";

export function ContradictionInbox() {
  const [items, setItems] = useState<Contradiction[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    setLoading(true);
    setError(null);
    api
      .contradictions("personal")
      .then(setItems)
      .catch((e) => setError(e instanceof Error ? e.message : "Failed"))
      .finally(() => setLoading(false));
  };

  useEffect(load, []);

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="animate-pulse-amber text-xl text-amber">◐</div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3">
        <p className="font-mono text-sm text-rose">{error}</p>
        <button
          onClick={load}
          className="rounded-lg border border-line px-3 py-1.5 font-mono text-xs text-text-dim hover:border-amber hover:text-amber"
        >
          retry
        </button>
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2">
        <div className="text-4xl text-sage opacity-40">✓</div>
        <p className="font-display text-lg text-text-dim">No contradictions</p>
        <p className="font-mono text-xs text-text-faint">
          All beliefs are consistent.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full overflow-auto p-8">
      <div className="mb-4 flex items-center justify-between">
        <p className="font-mono text-sm text-text-faint">
          {items.length} conflicting {items.length === 1 ? "statement" : "statements"}
        </p>
        <button
          onClick={load}
          className="rounded-lg border border-line px-3 py-1.5 font-mono text-xs text-text-dim hover:border-amber hover:text-amber"
        >
          refresh
        </button>
      </div>

      <div className="space-y-3 stagger">
        {items.map((c, i) => (
          <div
            key={i}
            className="rounded-xl border border-line bg-surface p-4"
          >
            <div className="mb-3 flex items-center gap-2">
              <span className="text-rose">⚡</span>
              <span className="font-display text-base text-text">
                {c.entity_surface}
              </span>
              <span className="rounded bg-surface-2 px-2 py-0.5 font-mono text-xs text-amber-dim">
                {c.predicate}
              </span>
            </div>

            <div className="space-y-1.5">
              {c.conflicting_values.map((val, vi) => (
                <div
                  key={vi}
                  className="flex items-center gap-2 rounded-lg bg-ink-2 px-3 py-2"
                >
                  <span className="text-rose">≠</span>
                  <span className="text-sm text-text-dim">{val}</span>
                </div>
              ))}
            </div>

            <div className="mt-3 flex gap-2">
              <button className="rounded-lg border border-line px-3 py-1.5 font-mono text-xs text-text-faint hover:border-sage hover:text-sage">
                keep first
              </button>
              <button className="rounded-lg border border-line px-3 py-1.5 font-mono text-xs text-text-faint hover:border-amber hover:text-amber">
                retract
              </button>
              <span className="ml-auto self-center font-mono text-xs text-text-faint">
                {c.statement_id.slice(0, 16)}…
              </span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
