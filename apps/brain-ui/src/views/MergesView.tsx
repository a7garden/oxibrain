import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { toast } from "sonner";
import type { MergeRecord, SearchResult } from "../api";
import { api } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { ErrorState } from "../components/ErrorState";
import { HUE_DOT, hueForType } from "../lib/hue";
import { fetchers, qk } from "../queries";

/** Compact display of an entity id. The full id travels in the `title`
 *  attribute so hover reveals it. The loser/winner columns carry EntityIds
 *  (no surface field on `MergeRecord`) so we always render the id; users
 *  follow the link to the entity page where the surface is available. */
function truncateEntityId(id: string): string {
  return id.length > 12 ? id.slice(0, 12) : id;
}

/** Local view of an entity picked from a search dropdown — the merge payload
 *  needs surface + type to round-trip through `merge_entities`. The id is
 *  kept around for invalidation of the graph/brief families after the write. */
interface PickedEntity {
  entityId: string;
  surface: string;
  entityType: string;
}

export function MergesView() {
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: qk.merges,
    queryFn: fetchers.merges,
    refetchInterval: 15_000,
  });

  // ── Picker state (winner / loser) ────────────────────────────────────
  const [winner, setWinner] = useState<PickedEntity | null>(null);
  const [loser, setLoser] = useState<PickedEntity | null>(null);
  const [confirming, setConfirming] = useState(false);

  const mutation = useMutation({
    mutationFn: () => {
      if (!winner || !loser) {
        // Defensive: button is disabled unless both are picked, but the
        // mutation body is the one place that can be wrong silently.
        throw new Error("Both winner and loser must be selected");
      }
      return api.mergeEntities(
        loser.surface,
        loser.entityType,
        winner.surface,
        winner.entityType,
      );
    },
    onSuccess: (serverText) => {
      toast.success(serverText || "Merge recorded");
      // Invalidate merges table + space overview (entity count drops by one).
      void queryClient.invalidateQueries({ queryKey: qk.merges });
      void queryClient.invalidateQueries({ queryKey: qk.space });
      // The loser's id stops resolving and the winner's graph gains the
      // loser's beliefs; invalidate both keyed families explicitly.
      if (loser) {
        void queryClient.invalidateQueries({ queryKey: qk.graph(loser.entityId) });
        void queryClient.invalidateQueries({ queryKey: qk.brief(loser.entityId) });
        void queryClient.invalidateQueries({ queryKey: qk.timeline(loser.entityId) });
      }
      if (winner) {
        void queryClient.invalidateQueries({ queryKey: qk.graph(winner.entityId) });
        void queryClient.invalidateQueries({ queryKey: qk.brief(winner.entityId) });
        void queryClient.invalidateQueries({ queryKey: qk.timeline(winner.entityId) });
      }
      // Reset the form.
      setWinner(null);
      setLoser(null);
      setConfirming(false);
    },
    onError: (e: unknown) => {
      toast.error(e instanceof Error ? e.message : String(e));
      setConfirming(false);
    },
  });

  const canSubmit = winner !== null && loser !== null && !mutation.isPending;

  return (
    <div className="mx-auto max-w-4xl p-8">
      <header className="mb-8">
        <h1 className="font-display text-2xl font-semibold text-text">Merges</h1>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          recorded entity merges — redirect one entity into another
        </p>
      </header>

      {/* New-merge form */}
      <section
        aria-label="New merge"
        className="mb-8 rounded-[var(--card-radius)] border border-line bg-surface-raised p-5"
      >
        <h2 className="font-display text-sm font-medium text-text">
          Record a new merge
        </h2>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          search for both entities, then confirm — the loser will be redirected
          into the winner.
        </p>

        <div className="mt-5 grid gap-4 md:grid-cols-[1fr_auto_1fr] md:items-start">
          <EntityPicker
            label="Winner"
            tone="success"
            value={winner}
            onChange={setWinner}
            placeholder="the entity to keep"
          />

          <div className="flex items-end justify-center pb-1">
            <button
              type="button"
              aria-label="Swap winner and loser"
              onClick={() => {
                setWinner((curW) => {
                  const oldW = curW;
                  setLoser(oldW);
                  return loser;
                });
              }}
              disabled={!winner && !loser}
              className="rounded-[var(--button-radius)] border border-line/50 px-3 py-2 font-mono text-base text-text-muted transition-colors hover:bg-surface-muted disabled:opacity-40"
            >
              ⇄
            </button>
          </div>

          <EntityPicker
            label="Loser"
            tone="error"
            value={loser}
            onChange={setLoser}
            placeholder="the entity to redirect"
          />
        </div>

        <div className="mt-5 flex items-center justify-end gap-3">
          <button
            type="button"
            onClick={() => {
              setWinner(null);
              setLoser(null);
            }}
            disabled={!winner && !loser}
            className="rounded-[var(--button-radius)] px-3 py-1.5 font-mono text-xs text-text-muted transition-colors hover:bg-surface-muted disabled:opacity-40"
          >
            clear
          </button>
          <button
            type="button"
            onClick={() => setConfirming(true)}
            disabled={!canSubmit}
            className="bg-interactive-primary text-interactive-primary-foreground rounded-[var(--button-radius)] px-4 py-2 font-mono text-xs font-medium transition-opacity hover:opacity-90 disabled:opacity-40"
          >
            {mutation.isPending ? "merging…" : "merge"}
          </button>
        </div>
      </section>

      {/* Recorded-merge table */}
      <section aria-label="Recorded merges">
        {query.isPending ? (
          <div className="space-y-2">
            <div className="skeleton h-10 w-full" />
            <div className="skeleton h-10 w-11/12" />
            <div className="skeleton h-10 w-10/12" />
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
          <p className="font-mono text-xs text-text-subtle">
            No merges recorded yet.
          </p>
        ) : (
          <MergesTable records={query.data} />
        )}
      </section>

      {confirming && winner && loser && (
        <ConfirmDialog
          titleId="merges-confirm-title"
          title="Record this merge?"
          description={
            <>
              The loser will be redirected to the winner — future lookups for
              the loser surface will resolve to the winner entity.
            </>
          }
          details={[
            { label: "loser", value: loser.surface },
            { label: "winner", value: winner.surface },
          ]}
          cancelLabel="cancel"
          confirmLabel="record merge"
          confirmingLabel="recording…"
          variant="primary"
          submitting={mutation.isPending}
          onCancel={() => setConfirming(false)}
          onConfirm={() => mutation.mutate()}
        />
      )}
    </div>
  );
}

// ── Entity picker ──────────────────────────────────────────────────────

interface EntityPickerProps {
  label: string;
  tone: "success" | "error";
  value: PickedEntity | null;
  onChange: (next: PickedEntity | null) => void;
  placeholder: string;
}

/** Search-driven entity picker. Debounces 300ms before hitting
 *  `api.search`; click a result to select. Selection shows as a chip with
 *  × to clear. The MERGE payload (surface + type) is captured at pick-time
 *  so the confirm call never re-resolves anything. */
function EntityPicker({
  label,
  tone,
  value,
  onChange,
  placeholder,
}: EntityPickerProps) {
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [open, setOpen] = useState(false);

  // Debounce 300ms — same settle window AskView uses, so the search cache
  // shape stays familiar.
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      setDebounced(query.trim());
    }, 300);
    return () => window.clearTimeout(timer.current);
  }, [query]);

  const search = useQuery({
    queryKey: qk.search(debounced),
    queryFn: () => fetchers.search(debounced),
    enabled: debounced.length > 0,
  });

  // Close the dropdown on outside click.
  const rootRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  const toneRing = tone === "success" ? "border-status-success/40" : "border-status-error/40";
  const toneChip = tone === "success" ? "bg-status-success-subtle text-status-success-on-subtle" : "bg-status-error-subtle text-status-error-on-subtle";

  return (
    <div className="min-w-0">
      <label className="font-mono text-2xs font-medium tracking-wider text-text-subtle uppercase">
        {label}
      </label>
      <div ref={rootRef} className={`mt-1.5 relative rounded-[var(--input-radius)] border ${toneRing} bg-surface shadow-[var(--input-shadow)]`}>
        {value ? (
          <div className="flex items-center gap-2 px-3.5 py-2">
            <span className={`h-2 w-2 shrink-0 rounded-full ${HUE_DOT[hueForType(value.entityType)]}`} />
            <span className="min-w-0 flex-1 truncate text-sm text-text">
              {value.surface}
            </span>
            <span className={`shrink-0 rounded-full px-2 py-0.5 font-mono text-2xs font-medium tracking-wider uppercase ${toneChip}`}>
              {value.entityType}
            </span>
            <button
              type="button"
              aria-label={`Clear ${label}`}
              onClick={() => onChange(null)}
              className="ml-1 shrink-0 rounded-full px-1 font-mono text-sm text-text-muted transition-colors hover:bg-surface-muted hover:text-text"
            >
              ×
            </button>
          </div>
        ) : (
          <input
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setOpen(true);
            }}
            onFocus={() => setOpen(true)}
            placeholder={placeholder}
            className="h-9 w-full rounded-[var(--input-radius)] bg-surface px-3.5 text-sm text-text placeholder:text-text-subtle focus:outline-none"
          />
        )}

        {open && !value && debounced.length > 0 && (
          <ul className="absolute left-0 right-0 top-full z-20 mt-1 max-h-72 overflow-auto rounded-[var(--card-radius)] border border-line bg-surface-raised shadow-lg">
            {search.isPending ? (
              <li className="px-3.5 py-3">
                <div className="skeleton h-4 w-full" />
              </li>
            ) : search.isError ? (
              <li className="px-3.5 py-2 font-mono text-xs text-status-error-on-subtle">
                {search.error instanceof Error
                  ? search.error.message
                  : String(search.error)}
              </li>
            ) : search.data.length === 0 ? (
              <li className="px-3.5 py-2 font-mono text-xs text-text-subtle">
                No matches.
              </li>
            ) : (
              search.data.map((hit) => (
                <SearchRow
                  key={hit.entity_id}
                  hit={hit}
                  onPick={(row) => {
                    onChange({
                      entityId: row.entity_id,
                      surface: row.entity_surface,
                      entityType: row.entity_type,
                    });
                    setQuery("");
                    setDebounced("");
                    setOpen(false);
                  }}
                />
              ))
            )}
          </ul>
        )}
      </div>
    </div>
  );
}

interface SearchRowProps {
  hit: SearchResult;
  onPick: (hit: SearchResult) => void;
}

function SearchRow({ hit, onPick }: SearchRowProps) {
  const hue = hueForType(hit.entity_type);
  return (
    <li>
      <button
        type="button"
        onClick={() => onPick(hit)}
        className="flex w-full items-center gap-2.5 px-3.5 py-2 text-left transition-colors hover:bg-surface-muted"
      >
        <span className={`h-2 w-2 shrink-0 rounded-full ${HUE_DOT[hue]}`} />
        <span className="min-w-0 flex-1 truncate text-sm text-text">
          {hit.entity_surface}
        </span>
        <span className="shrink-0 font-mono text-2xs uppercase tracking-wider text-text-subtle">
          {hit.entity_type}
        </span>
      </button>
    </li>
  );
}

// ── Table ──────────────────────────────────────────────────────────────

interface MergesTableProps {
  records: MergeRecord[];
}

function MergesTable({ records }: MergesTableProps) {
  // Newest first — decided_at is the server's wall-clock stamp for the
  // merge declaration.
  const sorted = useMemo(
    () => [...records].sort((a, b) => b.decided_at - a.decided_at),
    [records],
  );

  return (
    <div className="overflow-hidden rounded-[var(--card-radius)] border border-line bg-surface-raised">
      <table className="w-full table-fixed text-left">
        <thead>
          <tr className="border-b border-line/50 bg-surface-muted/40 font-mono text-2xs uppercase tracking-wider text-text-subtle">
            <th className="w-[35%] px-4 py-2.5">Loser → Winner</th>
            <th className="w-[18%] px-4 py-2.5">Decided by</th>
            <th className="w-[15%] px-4 py-2.5">Decided at</th>
            <th className="px-4 py-2.5">Provenance</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-line/50">
          {sorted.map((record) => (
            <tr key={record.id} className="text-sm">
              <td className="px-4 py-3">
                <div className="flex items-center gap-2">
                  <Link
                    to="/entity/$entityId"
                    params={{ entityId: record.loser }}
                    search={{ tab: "brief" }}
                    title={record.loser}
                    className="rounded-[var(--badge-radius)] bg-surface-muted px-2 py-0.5 font-mono text-xs text-text hover:bg-surface-sunken hover:underline"
                  >
                    {truncateEntityId(record.loser)}
                  </Link>
                  <span className="text-text-subtle">→</span>
                  <Link
                    to="/entity/$entityId"
                    params={{ entityId: record.winner }}
                    search={{ tab: "brief" }}
                    title={record.winner}
                    className="rounded-[var(--badge-radius)] bg-surface-muted px-2 py-0.5 font-mono text-xs text-text hover:bg-surface-sunken hover:underline"
                  >
                    {truncateEntityId(record.winner)}
                  </Link>
                </div>
              </td>
              <td className="px-4 py-3">
                <DecisionBadge kind={record.decided_by.kind} />
              </td>
              <td className="px-4 py-3 font-mono text-xs text-text-dim">
                {new Date(record.decided_at).toISOString().slice(0, 10)}
              </td>
              <td className="px-4 py-3">
                {record.provenance ? (
                  <span
                    className="font-mono text-xs text-text-dim"
                    title={record.provenance}
                  >
                    {truncateEntityId(record.provenance)}
                  </span>
                ) : (
                  <span className="font-mono text-xs text-text-subtle">—</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Compact badge for the MergeDecision kind — `user` is a deliberate human
 *  action (DESIGN §6.4 success-subtle); `rule` and `import` are background
 *  bookkeeping. */
function DecisionBadge({ kind }: { kind: "rule" | "user" | "import" }) {
  let classes: string;
  let label: string;
  if (kind === "user") {
    classes = "bg-status-success-subtle text-status-success-on-subtle";
    label = "user";
  } else if (kind === "rule") {
    classes = "bg-surface-muted text-text-muted";
    label = "rule";
  } else {
    classes = "bg-surface-muted text-text-muted";
    label = "import";
  }
  return (
    <span
      className={`inline-block rounded-full px-2 py-0.5 font-mono text-2xs font-medium tracking-wider uppercase ${classes}`}
    >
      {label}
    </span>
  );
}