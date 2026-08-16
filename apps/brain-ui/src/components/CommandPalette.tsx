import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import type { SearchResult } from "../api";
import { HUE_DOT, hueForType } from "../lib/hue";
import { fetchers, qk } from "../queries";
import { toggleTheme } from "../theme";

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
}

type Action =
  | { kind: "nav"; to: string; label: string; icon: string }
  | { kind: "theme"; label: string; icon: string };

/** Top-centered command palette (DESIGN §6.7 variant: top-center, not
 *  modal-centered). 7 static actions then debounced entity search rows. The
 *  list is a single flat list — active index wraps; Enter runs the action or
 *  navigates to /entity/$id. Esc closes; Tab cycles within the dialog; click
 *  or hover selects. */
export function CommandPalette({ open, onClose }: CommandPaletteProps) {
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const rowRefs = useRef<(HTMLButtonElement | null)[]>([]);
  // Reset state every time we open — keeps the palette from leaking the
  // prior query across sessions.
  const wasOpen = useRef(false);

  // Static actions — six navigation routes plus theme toggle. Order is
  // significant: the active-index walks this list first.
  const actions: readonly Action[] = useMemo(
    () => [
      { kind: "nav", to: "/", label: "Overview", icon: "◐" },
      { kind: "nav", to: "/graph", label: "Graph", icon: "✦" },
      { kind: "nav", to: "/ask", label: "Ask", icon: "⌕" },
      { kind: "nav", to: "/conflicts", label: "Conflicts", icon: "⚡" },
      { kind: "nav", to: "/merges", label: "Merges", icon: "⇄" },
      { kind: "nav", to: "/capture", label: "Capture", icon: "✎" },
      { kind: "theme", label: "Toggle theme", icon: "◑" },
    ],
    [],
  );

  // Debounced search — only fire when the query is non-empty. The brief
  // spec calls for 200ms; the daemon is fast enough that the perceived
  // latency is dominated by the debounce window, not the network.
  const [debounced, setDebounced] = useState("");
  useEffect(() => {
    if (!open) return;
    const handle = window.setTimeout(() => setDebounced(query.trim()), 200);
    return () => window.clearTimeout(handle);
  }, [query, open]);

  const searchQuery = useQuery({
    queryKey: qk.search(debounced),
    queryFn: () => fetchers.search(debounced),
    enabled: open && debounced.length > 0,
  });

  // Composite list: static actions (always present, hidden only when there
  // are no actions — n/a here) then entity results when the query is
  // non-empty. The active index walks the merged list.
  const entityResults: readonly SearchResult[] = useMemo(() => {
    if (debounced.length === 0) return [];
    return searchQuery.data ?? [];
  }, [debounced, searchQuery.data]);

  const total = actions.length + entityResults.length;

  // Reset on open, autofocus input.
  useEffect(() => {
    if (open && !wasOpen.current) {
      setQuery("");
      setDebounced("");
      setActive(0);
      // Defer focus so the panel has measured its layout first.
      window.setTimeout(() => inputRef.current?.focus(), 0);
    }
    wasOpen.current = open;
  }, [open]);

  // If the list shape changes (results arrive) keep the active index in
  // bounds.
  useEffect(() => {
    if (active >= total) setActive(total === 0 ? 0 : total - 1);
  }, [total, active]);

  // Keep the active row visible when the list overflows `max-h-[55vh]`.
  // `block: "nearest"` only scrolls when the row is outside the viewport.
  useEffect(() => {
    rowRefs.current[active]?.scrollIntoView({ block: "nearest" });
  }, [active]);

  // Dialog-scoped key handler. The useHotkeys hook already suppresses
  // single-key combos while the palette input is focused, so we own the
  // full key surface while the palette is open.
  const onDialogKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (total === 0) return;
      setActive((cur) => (cur + 1) % total);
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      if (total === 0) return;
      setActive((cur) => (cur - 1 + total) % total);
      return;
    }
    if (e.key === "Home") {
      e.preventDefault();
      setActive(0);
      return;
    }
    if (e.key === "End") {
      e.preventDefault();
      if (total === 0) return;
      setActive(total - 1);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      commit();
      return;
    }
    if (e.key === "Tab") {
      // Simple focus trap: bounce Tab between input and the list region.
      // The list region is non-focusable; we redirect to the input either
      // way so focus stays inside the dialog.
      e.preventDefault();
      inputRef.current?.focus();
    }
  };

  function commit() {
    if (total === 0) return;
    if (active < actions.length) {
      const action = actions[active];
      if (action.kind === "nav") {
        navigate({ to: action.to });
      } else {
        toggleTheme();
      }
      onClose();
      return;
    }
    const hit = entityResults[active - actions.length];
    if (!hit) return;
    navigate({
      to: "/entity/$entityId",
      params: { entityId: hit.entity_id },
      search: { tab: "brief" },
    });
    onClose();
  }

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 backdrop-blur-sm pt-[15vh]"
      onClick={(e) => {
        // Backdrop click closes; clicks inside the panel don't bubble here.
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onKeyDown={onDialogKey}
        className="bg-surface-raised text-text w-full max-w-lg rounded-[var(--dialog-radius)] shadow-lg overflow-hidden"
      >
        <div className="border-b border-line px-3.5 py-3">
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActive(0);
            }}
            placeholder="Search entities and actions…"
            aria-label="Command palette search"
            autoComplete="off"
            spellCheck={false}
            className="h-8 w-full bg-transparent text-sm text-text placeholder:text-text-subtle focus:outline-none"
          />
        </div>

        <ul
          // List region — non-focusable, but exists so screen readers can
          // announce the listbox container.
          aria-label="Commands"
          className="max-h-[55vh] overflow-y-auto py-1"
        >
          {actions.map((action, i) => {
            const isActive = i === active;
            return (
              <li key={action.label}>
                <button
                  ref={(el) => {
                    rowRefs.current[i] = el;
                  }}
                  type="button"
                  onMouseEnter={() => setActive(i)}
                  onClick={commit}
                  className={`flex w-full items-center gap-2.5 px-3.5 py-2 text-left transition-colors ${
                    isActive
                      ? "bg-surface-muted text-text"
                      : "text-text hover:bg-surface-muted/60"
                  }`}
                >
                  <span
                    className="w-5 shrink-0 text-center text-sm text-text-muted"
                    aria-hidden
                  >
                    {action.icon}
                  </span>
                  <span className="flex-1 truncate text-sm">{action.label}</span>
                </button>
              </li>
            );
          })}

          {debounced.length > 0 && (
            <>
              <li aria-hidden className="px-3.5 py-1.5">
                <p className="font-mono text-2xs font-medium tracking-wider uppercase text-text-subtle">
                  Entities
                </p>
              </li>
              {searchQuery.isPending ? (
                <li>
                  <div className="space-y-1 px-3.5 py-2">
                    <div className="skeleton h-7 w-full" />
                    <div className="skeleton h-7 w-11/12" />
                  </div>
                </li>
              ) : entityResults.length === 0 ? (
                <li>
                  <p className="px-3.5 py-3 text-sm text-text-muted">
                    No entities matched “{debounced}”.
                  </p>
                </li>
              ) : (
                entityResults.map((hit, i) => {
                  const idx = actions.length + i;
                  const hue = hueForType(hit.entity_type);
                  const isActive = idx === active;
                  return (
                    <li key={hit.entity_id}>
                      <button
                        ref={(el) => {
                          rowRefs.current[idx] = el;
                        }}
                        type="button"
                        onMouseEnter={() => setActive(idx)}
                        onClick={commit}
                        className={`flex w-full items-center gap-2.5 px-3.5 py-2 text-left transition-colors ${
                          isActive
                            ? "bg-surface-muted text-text"
                            : "text-text hover:bg-surface-muted/60"
                        }`}
                      >
                        <span
                          className={`h-2 w-2 shrink-0 rounded-full ${HUE_DOT[hue]}`}
                        />
                        <span className="min-w-0 flex-1 truncate text-sm">
                          {hit.entity_surface}
                        </span>
                        <span className="shrink-0 font-mono text-2xs uppercase tracking-wider text-text-subtle">
                          {hit.entity_type}
                        </span>
                      </button>
                    </li>
                  );
                })
              )}
            </>
          )}
        </ul>

        <div className="border-t border-line px-3.5 py-1.5 font-mono text-2xs text-text-subtle">
          <span>↑↓ navigate</span>
          <span className="mx-2">·</span>
          <span>↵ select</span>
          <span className="mx-2">·</span>
          <span>esc close</span>
        </div>
      </div>
    </div>
  );
}
