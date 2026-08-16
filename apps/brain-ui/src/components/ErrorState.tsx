interface ErrorStateProps {
  message: string;
  onRetry?: () => void;
}

/** Subtle-panel retry card. Uses status-error-subtle tokens so it never
 *  reads as a destructive modal. */
export function ErrorState({ message, onRetry }: ErrorStateProps) {
  return (
    <div className="bg-status-error-subtle text-status-error-on-subtle m-6 flex items-center justify-between gap-4 rounded-[var(--card-radius)] border border-line px-5 py-4">
      <div>
        <p className="font-display text-base font-medium">Something went wrong</p>
        <p className="mt-1 font-mono text-xs opacity-90">{message}</p>
      </div>
      {onRetry && (
        <button
          onClick={onRetry}
          className="rounded-[var(--button-radius)] border border-line/50 px-3 py-1.5 font-mono text-xs transition-colors hover:bg-status-error-subtle/80"
        >
          retry
        </button>
      )}
    </div>
  );
}