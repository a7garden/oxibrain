import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { toast } from "sonner";
import { api, type ContradictionDetail } from "../api";
import { ErrorState } from "../components/ErrorState";
import { HUE_DOT, hueForType } from "../lib/hue";
import { fetchers, qk } from "../queries";

interface ConflictValue {
  detail: ContradictionDetail;
  /** All episode ids supporting this value (affirm + deny), flattened for
   *  display. Used to render the evidence-chip row and to count "n episodes"
   *  on the retract dialog. */
  episodes: Array<{ episodeId: string; polarity: "affirm" | "deny" }>;
}

interface ConflictGroup {
  key: string;
  subjectId: string;
  subjectSurface: string;
  subjectType: string;
  predicate: string;
  values: ConflictValue[];
}

/** Group server details client-side: one card per (subject_id, predicate).
 *  Episodes are flattened per value purely for display — the retract button
 *  is statement-level (one per value row), so per-chip retract was a fiction. */
function groupConflicts(details: ContradictionDetail[]): ConflictGroup[] {
  const groups = new Map<string, ConflictGroup>();
  for (const d of details) {
    const key = `${d.subject_id}|${d.predicate}`;
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        subjectId: d.subject_id,
        subjectSurface: d.subject_surface,
        subjectType: d.subject_type,
        predicate: d.predicate,
        values: [],
      };
      groups.set(key, group);
    }
    group.values.push({
      detail: d,
      episodes: [
        ...d.affirm_episodes.map((id) => ({ episodeId: id, polarity: "affirm" as const })),
        ...d.deny_episodes.map((id) => ({ episodeId: id, polarity: "deny" as const })),
      ],
    });
  }
  return Array.from(groups.values()).sort((a, b) => {
    const bySub = a.subjectSurface.localeCompare(b.subjectSurface);
    return bySub !== 0 ? bySub : a.predicate.localeCompare(b.predicate);
  });
}

/** Compact display of an episode id. The full id travels in the `title`
 *  attribute so hover reveals it. Constant `10` matches the house log
 *  prefix; the chip stays one line in the value row. */
function truncateEpisodeId(id: string): string {
  return id.length > 10 ? id.slice(0, 10) : id;
}

export function ConflictsView() {
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: qk.contradictions,
    queryFn: fetchers.contradictions,
    refetchInterval: 15_000,
  });

  const groups = useMemo(
    () => (query.data ? groupConflicts(query.data) : []),
    [query.data],
  );

  const [pending, setPending] = useState<PendingRetract | null>(null);

  const mutation = useMutation({
    mutationFn: (statementId: string) => api.retractStatement(statementId),
    onSuccess: () => {
      toast.success("Retracted — declaration episode recorded");
      void queryClient.invalidateQueries({ queryKey: qk.contradictions });
      void queryClient.invalidateQueries({ queryKey: qk.space });
    },
    onError: (e: unknown) => {
      toast.error(e instanceof Error ? e.message : String(e));
    },
  });

  return (
    <div className="mx-auto max-w-3xl p-8">
      <header className="mb-8">
        <h1 className="font-display text-2xl font-semibold text-text">
          Conflicts
        </h1>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          statements with contradicting evidence — retract one value at a time
        </p>
      </header>

      {query.isPending ? (
        <div className="space-y-4">
          <div className="skeleton h-32 w-full" />
          <div className="skeleton h-32 w-full" />
          <div className="skeleton h-32 w-full" />
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
      ) : groups.length === 0 ? (
        <p className="text-sm text-status-success-on-subtle">
          No known conflicts — every contradicted statement is resolved.
        </p>
      ) : (
        <ul className="space-y-4">
          {groups.map((group) => (
            <li key={group.key}>
              <ConflictCard
                group={group}
                onRetract={(value) => {
                  const d = value.detail;
                  setPending({
                    statementId: d.statement_id,
                    subjectSurface: d.subject_surface,
                    predicate: d.predicate,
                    objectValue: d.object_value,
                    objectKind: d.object_kind,
                    episodeCount: value.episodes.length,
                  });
                }}
              />
            </li>
          ))}
        </ul>
      )}

      {pending && (
        <ConfirmRetractDialog
          pending={pending}
          submitting={mutation.isPending}
          onCancel={() => setPending(null)}
          onConfirm={() => {
            const sid = pending.statementId;
            mutation.mutate(sid, {
              onSettled: () => setPending(null),
            });
          }}
        />
      )}
    </div>
  );
}

interface ConflictCardProps {
  group: ConflictGroup;
  onRetract: (value: ConflictValue) => void;
}

function ConflictCard({ group, onRetract }: ConflictCardProps) {
  const hue = hueForType(group.subjectType);
  return (
    <article className="rounded-[var(--card-radius)] border border-line bg-surface-raised">
      <header className="flex items-center gap-3 border-b border-line/50 px-5 py-3">
        <span className={`h-2 w-2 shrink-0 rounded-full ${HUE_DOT[hue]}`} />
        <Link
          to="/entity/$entityId"
          params={{ entityId: group.subjectId }}
          search={{ tab: "brief" }}
          className="text-sm font-medium text-text hover:underline"
        >
          {group.subjectSurface}
        </Link>
        <span className="font-mono text-xs text-text-muted">
          {group.predicate}
        </span>
        <span className="ml-auto shrink-0 rounded-[var(--badge-radius)] bg-status-warning-subtle px-2 py-0.5 font-mono text-2xs font-medium tracking-wider text-status-warning-on-subtle uppercase">
          {group.values.length} value{group.values.length === 1 ? "" : "s"}
        </span>
      </header>
      <ul className="divide-y divide-line/50">
        {group.values.map((value) => (
          <ValueRow
            key={value.detail.statement_id}
            value={value}
            onRetract={onRetract}
          />
        ))}
      </ul>
    </article>
  );
}

interface ValueRowProps {
  value: ConflictValue;
  onRetract: (value: ConflictValue) => void;
}

function ValueRow({ value, onRetract }: ValueRowProps) {
  const { detail, episodes } = value;
  return (
    <li className="flex items-start justify-between gap-4 px-5 py-4">
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="text-sm text-text">{detail.object_value}</span>
          {detail.object_kind === "entity" && (
            <span className="rounded-[var(--badge-radius)] bg-surface-muted px-2 py-0.5 font-mono text-2xs uppercase tracking-wider text-text-muted">
              entity
            </span>
          )}
        </div>
        {episodes.length > 0 && (
          <ul className="mt-3 flex flex-wrap gap-2">
            {episodes.map((ep) => (
              <li key={`${ep.polarity}-${ep.episodeId}`}>
                <span className="inline-flex items-center gap-1.5 rounded-[var(--badge-radius)] border border-line/50 bg-surface-muted px-2 py-1">
                  <span
                    className="font-mono text-2xs text-text"
                    title={ep.episodeId}
                  >
                    {truncateEpisodeId(ep.episodeId)}
                  </span>
                  <span className="font-mono text-2xs uppercase tracking-wider text-text-subtle">
                    {ep.polarity}
                  </span>
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <button
        type="button"
        onClick={() => onRetract(value)}
        className="shrink-0 rounded-[var(--button-radius)] border border-line/50 px-2.5 py-1 font-mono text-2xs text-text-muted transition-colors hover:bg-surface-muted"
      >
        retract
      </button>
    </li>
  );
}

interface PendingRetract {
  statementId: string;
  subjectSurface: string;
  predicate: string;
  objectValue: string;
  objectKind: "entity" | "literal";
  episodeCount: number;
}

interface ConfirmRetractDialogProps {
  pending: PendingRetract;
  submitting: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

/** Confirm dialog matching DESIGN §6.7: backdrop blur+dark, surface-raised
 *  panel, destructive primary button, ghost cancel. Escape + backdrop click
 *  cancel; focus auto-routes to the destructive button on open. */
function ConfirmRetractDialog({
  pending,
  submitting,
  onCancel,
  onConfirm,
}: ConfirmRetractDialogProps) {
  const confirmRef = useRef<HTMLButtonElement | null>(null);
  const focusedOnce = useRef(false);

  useEffect(() => {
    if (!focusedOnce.current) {
      confirmRef.current?.focus();
      focusedOnce.current = true;
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !submitting) onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel, submitting]);
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 backdrop-blur-sm"
      onClick={(e) => {
        if (e.target === e.currentTarget && !submitting) onCancel();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="conflicts-retract-title"
        className="bg-surface-raised text-text w-full max-w-[520px] rounded-[var(--dialog-radius)] shadow-lg p-6"
      >
        <h2
          id="conflicts-retract-title"
          className="font-display text-base font-semibold text-text"
        >
          Retract this value?
        </h2>
        <p className="mt-2 text-sm text-text-muted">
          A new declaration episode will be recorded
          {pending.episodeCount === 1
            ? " denying the existing assertion of this statement."
            : ` denying all ${pending.episodeCount} assertions of this statement.`}
          {" "}The original episodes are preserved.
        </p>
        <dl className="mt-4 space-y-1.5 font-mono text-xs">
          <div className="flex justify-between gap-4">
            <dt className="text-text-subtle">subject</dt>
            <dd className="text-text">{pending.subjectSurface}</dd>
          </div>
          <div className="flex justify-between gap-4">
            <dt className="text-text-subtle">predicate</dt>
            <dd className="text-text">{pending.predicate}</dd>
          </div>
          <div className="flex justify-between gap-4">
            <dt className="text-text-subtle">object</dt>
            <dd className="text-text">
              {pending.objectValue}
              <span className="ml-2 text-text-subtle">({pending.objectKind})</span>
            </dd>
          </div>
        </dl>
        <div className="mt-6 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            disabled={submitting}
            className="rounded-[var(--button-radius)] px-3 py-1.5 font-mono text-xs text-text-muted transition-colors hover:bg-surface-muted disabled:opacity-50"
          >
            cancel
          </button>
          <button
            ref={confirmRef}
            type="button"
            onClick={onConfirm}
            disabled={submitting}
            className="bg-status-error text-text-inverse rounded-[var(--button-radius)] px-3 py-1.5 font-mono text-xs font-medium transition-opacity hover:opacity-90 disabled:opacity-50"
          >
            {submitting ? "retracting…" : "retract"}
          </button>
        </div>
      </div>
    </div>
  );
}
