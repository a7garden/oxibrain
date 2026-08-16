import type { Belief, SearchResult } from "../api";
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { ErrorState } from "../components/ErrorState";
import { HUE_DOT, hueForType } from "../lib/hue";
import { fetchers, qk } from "../queries";

/** Ask view — search with per-belief provenance.
 *
 * The input drives the `/ask?q=` search param (debounced, replace) so results
 * survive reload and Back/Forward. Each result expands to the entity's
 * beliefs; each belief discloses its `why` block — assertions with episode,
 * extractor, polarity, and the confidence breakdown. */
export function AskView() {
  const { q, autofocus } = useSearch({ from: "/ask" });
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [input, setInput] = useState(q);
  const [expanded, setExpanded] = useState<string | null>(null);

  // Debounced URL sync: typing settles 300ms before the URL moves.
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      if (input !== q) {
        void navigate({
          to: "/ask",
          search: { q: input, autofocus: 0 },
          replace: true,
        });
      }
    }, 300);
    return () => window.clearTimeout(timer.current);
  }, [input, q, navigate]);

  // External `q` changes (Back/Forward, deep link) re-sync the input.
  useEffect(() => {
    setInput((current) => (current === q ? current : q));
  }, [q]);

  // `autofocus` is a fresh timestamp pushed by the `/` hotkey on every
  // press. Any positive value triggers focus + select. The flag is
  // observable per-press (each push is a new number), so same-route `/`
  // presses refocus without navigating to clear.
  const lastAutofocus = useRef(0);
  useEffect(() => {
    if (!autofocus || autofocus === lastAutofocus.current) return;
    lastAutofocus.current = autofocus;
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [autofocus]);

  const query = useQuery({
    queryKey: qk.search(q),
    queryFn: () => fetchers.search(q),
    enabled: q.length > 0,
  });

  return (
    <div className="mx-auto max-w-3xl p-8">
      <h1 className="font-display text-2xl font-semibold text-text">Ask</h1>
      <p className="mt-1 mb-4 text-sm text-text-muted">
        Search entities; open a belief to see where it came from.
      </p>
      <input
        ref={inputRef}
        id="ask-input"
        autoFocus
        value={input}
        onChange={(e) => setInput(e.target.value)}
        placeholder="Search your brain…"
        aria-label="Search entities"
        className="h-9 w-full px-3.5 rounded-[var(--input-radius)] bg-surface text-text text-sm placeholder:text-text-subtle shadow-[var(--input-shadow)] focus-visible:shadow-[var(--input-shadow-focus)] focus-visible:outline-none"
      />

      <div className="mt-6">
        {q.length === 0 ? (
          <p className="text-sm text-text-muted">Type to search.</p>
        ) : query.isPending ? (
          <div className="space-y-2">
            <div className="skeleton h-12 w-full" />
            <div className="skeleton h-12 w-11/12" />
            <div className="skeleton h-12 w-10/12" />
          </div>
        ) : query.isError ? (
          <ErrorState
            message={
              query.error instanceof Error
                ? query.error.message
                : String(query.error)
            }
            onRetry={() => query.refetch()}
          />
        ) : query.data.length === 0 ? (
          <p className="text-sm text-text-muted">
            No entities matched “{q}”.
          </p>
        ) : (
          <ul className="space-y-2">
            {query.data.map((hit) => (
              <li key={hit.entity_id}>
                <ResultRow
                  hit={hit}
                  open={expanded === hit.entity_id}
                  onToggle={() =>
                    setExpanded((cur) =>
                      cur === hit.entity_id ? null : hit.entity_id,
                    )
                  }
                />
                {expanded === hit.entity_id && (
                  <BeliefsPanel entityId={hit.entity_id} />
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function ResultRow({
  hit,
  open,
  onToggle,
}: {
  hit: SearchResult;
  open: boolean;
  onToggle: () => void;
}) {
  const hue = hueForType(hit.entity_type);
  return (
    <button
      onClick={onToggle}
      aria-expanded={open}
      className="flex w-full items-center gap-3 rounded-[var(--card-radius)] border border-line bg-surface-raised px-4 py-3 text-left transition-colors hover:bg-surface-muted"
    >
      <span className={`h-2 w-2 shrink-0 rounded-full ${HUE_DOT[hue]}`} />
      <span className="min-w-0 flex-1">
        <span className="flex items-baseline gap-2">
          <span className="truncate text-sm font-medium text-text">
            {hit.entity_surface}
          </span>
          <span className="text-2xs uppercase tracking-wider text-text-subtle">
            {hit.entity_type}
          </span>
        </span>
        {hit.snippet && (
          <span className="mt-0.5 block truncate text-sm text-text-muted">
            {hit.snippet}
          </span>
        )}
      </span>
      <span className="shrink-0 rounded-[var(--badge-radius)] bg-surface-muted px-2 py-0.5 font-mono text-2xs text-text-muted">
        {hit.score.toFixed(2)}
      </span>
      <svg
        viewBox="0 0 16 16"
        className={`h-3.5 w-3.5 shrink-0 text-text-subtle transition-transform ${open ? "rotate-90" : ""}`}
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
      >
        <path d="M6 4l4 4-4 4" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </button>
  );
}

function BeliefsPanel({ entityId }: { entityId: string }) {
  const query = useQuery({
    queryKey: qk.beliefs(entityId),
    queryFn: () => fetchers.beliefs(entityId),
  });
  const [whyOpen, setWhyOpen] = useState<string | null>(null);

  if (query.isPending) {
    return (
      <div className="mt-2 space-y-2 rounded-[var(--card-radius)] border border-line bg-surface-raised p-4">
        <div className="skeleton h-4 w-5/6" />
        <div className="skeleton h-4 w-2/3" />
      </div>
    );
  }
  if (query.isError) {
    return (
      <div className="mt-2">
        <ErrorState
          message={
            query.error instanceof Error
              ? query.error.message
              : String(query.error)
          }
          onRetry={() => query.refetch()}
        />
      </div>
    );
  }
  if (query.data.length === 0) {
    return (
      <p className="mt-2 px-1 text-sm text-text-muted">
        No beliefs for this entity yet.
      </p>
    );
  }

  return (
    <div className="mt-2 space-y-1.5 rounded-[var(--card-radius)] border border-line bg-surface-raised p-4">
      {query.data.map((belief) => (
        <BeliefRow
          key={belief.statement}
          belief={belief}
          open={whyOpen === belief.statement}
          onToggle={() =>
            setWhyOpen((cur) =>
              cur === belief.statement ? null : belief.statement,
            )
          }
        />
      ))}
    </div>
  );
}

function BeliefRow({
  belief,
  open,
  onToggle,
}: {
  belief: Belief;
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <div>
      <button
        onClick={onToggle}
        aria-expanded={open}
        className="flex w-full items-center gap-2 rounded-[var(--badge-radius)] px-2 py-1.5 text-left transition-colors hover:bg-surface-muted"
      >
        <span className="truncate font-mono text-xs text-text">
          {belief.statement}
        </span>
        <span className="ml-auto shrink-0 font-mono text-2xs text-text-muted">
          {Math.round(belief.confidence * 100)}%
        </span>
        <StatusBadge status={belief.status} />
        <svg
          viewBox="0 0 16 16"
          className={`h-3 w-3 shrink-0 text-text-subtle transition-transform ${open ? "rotate-90" : ""}`}
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
        >
          <path d="M6 4l4 4-4 4" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
      {open && <WhyPanel statementId={belief.statement} />}
    </div>
  );
}

function WhyPanel({ statementId }: { statementId: string }) {
  const query = useQuery({
    queryKey: qk.why(statementId),
    queryFn: () => fetchers.why(statementId),
  });

  if (query.isPending) {
    return (
      <div className="space-y-1.5 px-4 py-2">
        <div className="skeleton h-3 w-full" />
        <div className="skeleton h-3 w-4/5" />
        <div className="skeleton h-3 w-3/5" />
      </div>
    );
  }
  if (query.isError) {
    return (
      <div className="px-4 py-2">
        <ErrorState
          message={
            query.error instanceof Error
              ? query.error.message
              : String(query.error)
          }
          onRetry={() => query.refetch()}
        />
      </div>
    );
  }

  const { statement, assertions, confidence_breakdown } = query.data;
  return (
    <div className="px-4 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm text-text">
          {statement.predicate} → {renderObject(statement.object)}
        </span>
        <span className="shrink-0 rounded-[var(--badge-radius)] bg-surface-muted px-2 py-0.5 font-mono text-2xs text-text-muted">
          support {confidence_breakdown.support_count} · contradicts{" "}
          {confidence_breakdown.contradiction_count}
        </span>
      </div>
      <ul className="mt-2 space-y-1">
        {assertions.map((a) => (
          <li
            key={a.assertion_id}
            className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 font-mono text-xs text-text-muted"
          >
            <span className="truncate text-text-subtle">{a.episode_id}</span>
            <span>{a.extractor ?? "declared"}</span>
            <span>{a.polarity}</span>
            <span>{Math.round(a.confidence * 100)}%</span>
            <span>{formatDate(a.recorded_at)}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Compact one-line rendering for a `Statement.object` projection:
 *  entity objects carry {kind, id, surface} — prefer the surface. Literal
 *  objects arrive as tagged JSON ({type, value}); render the plain value
 *  and fall back to raw JSON for exotic kinds. */
function renderObject(object: unknown): string {
  if (typeof object !== "object" || object === null) {
    return JSON.stringify(object);
  }
  const o = object as Record<string, unknown>;
  if (o.kind === "entity") {
    if (typeof o.surface === "string" && o.surface) return o.surface;
    return String(o.id);
  }
  if (typeof o.type === "string" && "value" in o) {
    return String(o.value);
  }
  return JSON.stringify(object);
}
function StatusBadge({ status }: { status: string }) {
  let classes: string;
  if (status === "contradicted") {
    classes = "bg-status-error-subtle text-status-error-on-subtle";
  } else if (status === "active") {
    classes = "bg-status-success-subtle text-status-success-on-subtle";
  } else {
    classes = "bg-surface-muted text-text-muted";
  }
  return (
    <span
      className={`shrink-0 rounded-full px-2 py-0.5 font-mono text-2xs font-medium tracking-wider uppercase ${classes}`}
    >
      {status}
    </span>
  );
}

/** Render epoch-ms as `YYYY-MM-DD`. */
function formatDate(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}
