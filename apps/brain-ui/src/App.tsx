import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { type ReactNode } from "react";
import { fetchers, qk } from "./queries";
import { toggleTheme } from "./theme";

// ── Sidebar primitives (DESIGN §6.11, verbatim) ────────────────────────

const sidebarPrimitives = {
  itemBase:
    "flex items-center w-full text-sm py-2 px-2 gap-3 rounded-md transition-colors",
  itemActive: "bg-surface-muted text-text font-medium",
  itemInactive: "text-text-muted hover:text-text hover:bg-surface-muted/50",
  sectionHeader:
    "px-2 py-1.5 text-2xs font-medium tracking-wider uppercase text-text-subtle",
} as const;

interface NavItem {
  to: "/" | "/graph" | "/ask" | "/conflicts" | "/merges" | "/capture";
  label: string;
  icon: string;
}

const NAV_ITEMS: readonly NavItem[] = [
  { to: "/", label: "Overview", icon: "◐" },
  { to: "/graph", label: "Graph", icon: "✦" },
  { to: "/ask", label: "Ask", icon: "⌕" },
  { to: "/conflicts", label: "Conflicts", icon: "⚡" },
  { to: "/merges", label: "Merges", icon: "⇄" },
  { to: "/capture", label: "Capture", icon: "✎" },
] as const;

interface AppShellProps {
  children: ReactNode;
}

/** Sidebar + offline banner + outlet wrapper. */
export function AppShell({ children }: AppShellProps) {
  const spaceQuery = useQuery({
    queryKey: qk.space,
    queryFn: fetchers.space,
    refetchInterval: 30_000,
  });

  const isOffline = spaceQuery.isError;
  const overview = spaceQuery.data;

  return (
    <div className="flex h-full font-sans">
      <aside className="bg-surface-sunken flex w-60 flex-col border-r border-line">
        <div className="flex items-center gap-2.5 px-5 py-5">
          <span className="text-hue-amber text-xl" aria-hidden>
            ◐
          </span>
          <h1 className="font-display text-lg font-semibold tracking-tight text-text">
            oxibrain
          </h1>
        </div>

        <nav className="flex flex-col gap-0.5 px-3">
          <p className={sidebarPrimitives.sectionHeader}>Space</p>
          {NAV_ITEMS.map((item) => {
            return (
              <Link
                key={item.to}
                to={item.to}
                activeProps={{ className: sidebarPrimitives.itemActive }}
                inactiveProps={{ className: sidebarPrimitives.itemInactive }}
                className={sidebarPrimitives.itemBase}
                activeOptions={{ exact: item.to === "/" }}
              >
                <span className="w-5 text-center text-sm" aria-hidden>
                  {item.icon}
                </span>
                <span>{item.label}</span>
              </Link>
            );
          })}

          <p className={`${sidebarPrimitives.sectionHeader} mt-3`}>Session</p>
          <button
            onClick={() => toggleTheme()}
            className={`${sidebarPrimitives.itemBase} ${sidebarPrimitives.itemInactive}`}
          >
            <span className="w-5 text-center text-sm" aria-hidden>
              ◑
            </span>
            <span>Toggle theme</span>
          </button>
        </nav>

        <div className="mt-auto border-t border-line/50 px-5 py-4">
          {overview ? (
            <dl className="space-y-1 font-mono text-xs text-text-subtle">
              <div className="flex justify-between">
                <dt>entities</dt>
                <dd className="text-text">{overview.entity_count}</dd>
              </div>
              <div className="flex justify-between">
                <dt>episodes</dt>
                <dd className="text-text">{overview.episode_count}</dd>
              </div>
              <div className="flex justify-between">
                <dt>conflicts</dt>
                <dd
                  className={
                    overview.contradiction_count > 0
                      ? "text-status-error"
                      : "text-text"
                  }
                >
                  {overview.contradiction_count}
                </dd>
              </div>
            </dl>
          ) : (
            <div className="font-mono text-xs text-text-subtle">—</div>
          )}
          <div className="mt-3 flex items-center gap-2">
            <span
              className={`h-1.5 w-1.5 rounded-full ${
                isOffline ? "bg-status-error" : "bg-status-success"
              }`}
            />
            <span className="font-mono text-xs text-text-subtle">
              {isOffline ? "offline" : "connected"}
            </span>
          </div>
        </div>
      </aside>

      <main className="flex flex-1 flex-col overflow-hidden">
        {isOffline && (
          <div className="bg-status-error-subtle text-status-error-on-subtle flex items-center justify-between gap-3 px-6 py-2.5 font-mono text-xs">
            <span>daemon unreachable</span>
            <button
              onClick={() => spaceQuery.refetch()}
              className="rounded-[var(--button-radius)] border border-line/50 px-2 py-0.5 transition-colors hover:bg-status-error-subtle/80"
            >
              retry
            </button>
          </div>
        )}
        <div className="flex-1 overflow-auto">{children}</div>
      </main>
    </div>
  );
}
