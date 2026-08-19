import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { ErrorState } from "../components/ErrorState";
import { rpc } from "../api";
import { fetchers, qk } from "../queries";

/** Raw JSON-RPC shape for `reproject`. The daemon does not yet expose
 *  this method (it's tracked as a follow-up to Plan D — Task 3 covers
 *  `console` tool sections; the `reproject` RPC method lands with the
 *  CLI/daemon parity work). The button wires the call so it ships
 *  ready; until the server lands the method, the daemon returns
 *  `METHOD_NOT_FOUND` and we surface that via toast rather than
 *  presenting a fake success. */
interface ReprojectResult {
  /** Wall-clock millis when reprojection completed. */
  completed_at: number;
  /** Number of entities reprojected. */
  entities_reprojected: number;
  /** Number of statements updated. */
  statements_updated: number;
}

export function OperationsView() {
  const queryClient = useQueryClient();

  const stats = useQuery({
    queryKey: qk.operations,
    queryFn: fetchers.operations,
    refetchInterval: 15_000,
  });

  const [confirming, setConfirming] = useState(false);

  const mutation = useMutation({
    mutationFn: (): Promise<ReprojectResult> =>
      // Direct JSON-RPC call — `reproject` is intentionally not exposed
      // as an MCP tool (too destructive for agent access). The rpc()
      // helper is already exported from api.ts; the server contract is
      // tracked in plan §Design Decisions #7.
      rpc<ReprojectResult>("reproject"),
    onSuccess: (result) => {
      toast.success(
        `Reprojected ${result.entities_reprojected} entities / ${result.statements_updated} statements`,
      );
      // Reprojection changes entity embeddings, statement projections,
      // and the resolution cache — refresh everything that depends on
      // them. The fetcher aliases operations → space, so we hit that
      // key directly; the rest use the global invalidate helper.
      void queryClient.invalidateQueries({ queryKey: qk.space });
      void queryClient.invalidateQueries({ queryKey: qk.contradictions });
      void queryClient.invalidateQueries({ queryKey: qk.merges });
      void queryClient.invalidateQueries({ queryKey: qk.operations });
    },
    onError: (e: unknown) => {
      toast.error(e instanceof Error ? e.message : String(e));
    },
  });

  return (
    <div className="mx-auto max-w-3xl p-8">
      <header className="mb-8">
        <h1 className="font-display text-2xl font-semibold text-text">
          Operations
        </h1>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          space statistics and destructive maintenance operations
        </p>
      </header>

      {stats.isPending ? (
        <div className="space-y-3">
          <div className="skeleton h-24 w-full" />
          <div className="skeleton h-24 w-full" />
        </div>
      ) : stats.isError ? (
        <ErrorState
          message={
            stats.error instanceof Error
              ? stats.error.message
              : String(stats.error)
          }
          onRetry={() => stats.refetch()}
        />
      ) : (
        <>
          <StatsPanel
            entityCount={stats.data.entity_count}
            episodeCount={stats.data.episode_count}
            contradictionCount={stats.data.contradiction_count}
          />

          <section
            aria-label="Reproject"
            className="mt-8 rounded-[var(--card-radius)] border border-line bg-surface-raised p-5"
          >
            <h2 className="font-display text-base font-medium text-text">
              Reproject
            </h2>
            <p className="mt-1 font-mono text-xs text-text-subtle">
              walk the entity / statement graph and refresh embeddings +
              projections. Use after bulk edits or when projections look
              stale. Heavy operation — runtime scales with entity count.
            </p>
            <button
              type="button"
              onClick={() => setConfirming(true)}
              disabled={mutation.isPending}
              className="mt-4 inline-flex items-center rounded-[var(--button-radius)] bg-interactive-primary px-3.5 py-1.5 font-mono text-xs font-medium text-interactive-primary-foreground transition-colors hover:bg-interactive-primary/90 disabled:opacity-50"
            >
              reproject
            </button>
          </section>
        </>
      )}

      {confirming && (
        <ConfirmDialog
          titleId="operations-reproject-title"
          title="Run reprojection?"
          description={
            <>
              This walks the entity graph and refreshes embeddings +
              projections for every entity. Runtime scales with entity
              count — a 10k-entity space typically takes a few minutes.
              Statement counts and contradictions will refresh on the
              Overview after completion.
            </>
          }
          details={[
            {
              label: "operation",
              value: <span className="font-mono">reproject</span>,
            },
            {
              label: "transport",
              value: <span className="font-mono">JSON-RPC</span>,
            },
          ]}
          cancelLabel="cancel"
          confirmLabel="reproject"
          confirmingLabel="reprojecting…"
          variant="primary"
          submitting={mutation.isPending}
          onCancel={() => {
            if (!mutation.isPending) setConfirming(false);
          }}
          onConfirm={() => {
            mutation.mutate(undefined, {
              onSettled: () => setConfirming(false),
            });
          }}
        />
      )}
    </div>
  );
}

interface StatsPanelProps {
  entityCount: number;
  episodeCount: number;
  contradictionCount: number;
}

function StatsPanel({
  entityCount,
  episodeCount,
  contradictionCount,
}: StatsPanelProps) {
  return (
    <section
      aria-label="Space statistics"
      className="grid grid-cols-3 gap-4 rounded-[var(--card-radius)] border border-line bg-surface-raised p-5"
    >
      <Stat label="entities" value={entityCount} />
      <Stat label="episodes" value={episodeCount} />
      <Stat
        label="conflicts"
        value={contradictionCount}
        tone={contradictionCount > 0 ? "error" : "neutral"}
      />
    </section>
  );
}

function Stat({
  label,
  value,
  tone = "neutral",
}: {
  label: string;
  value: number;
  tone?: "neutral" | "error";
}) {
  return (
    <div>
      <p className="font-mono text-2xs uppercase tracking-wider text-text-subtle">
        {label}
      </p>
      <p
        className={`mt-1 font-display text-2xl font-semibold ${
          tone === "error" ? "text-status-error" : "text-text"
        }`}
      >
        {value}
      </p>
    </div>
  );
}