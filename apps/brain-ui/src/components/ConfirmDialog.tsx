import { useEffect, useRef, type ReactNode } from "react";

interface ConfirmDialogProps {
  /** Title rendered at the top of the panel. */
  title: string;
  /** Body text shown below the title. */
  description: ReactNode;
  /** Optional definition list (label/value rows) shown above the buttons. */
  details?: Array<{ label: string; value: ReactNode }>;
  /** Cancel button label. */
  cancelLabel?: string;
  /** Confirm button label. */
  confirmLabel: string;
  /** Label while the mutation is in flight. */
  confirmingLabel?: string;
  /** Whether the mutation is currently running — disables both buttons and
   *  blocks Escape / backdrop cancel. */
  submitting: boolean;
  /** Visual style of the confirm button. `danger` for destructive actions
   *  (red `bg-status-error`); `primary` for ordinary writes. */
  variant?: "danger" | "primary";
  onCancel: () => void;
  onConfirm: () => void;
  /** ARIA id for the dialog title — wired to aria-labelledby. */
  titleId: string;
}

/** Confirm dialog matching DESIGN §6.7: backdrop blur+dark, surface-raised
 *  panel, primary button (interactive-primary or status-error), ghost cancel.
 *  Escape + backdrop click cancel; focus auto-routes to the confirm button
 *  on open (once, to survive parent re-renders that would otherwise reset
 *  focus — see T10 review). */
export function ConfirmDialog({
  title,
  description,
  details,
  cancelLabel = "cancel",
  confirmLabel,
  confirmingLabel,
  submitting,
  variant = "primary",
  onCancel,
  onConfirm,
  titleId,
}: ConfirmDialogProps) {
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

  const confirmClasses =
    variant === "danger"
      ? "bg-status-error text-text-inverse"
      : "bg-interactive-primary text-interactive-primary-foreground";

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
        aria-labelledby={titleId}
        className="bg-surface-raised text-text w-full max-w-[520px] rounded-[var(--dialog-radius)] shadow-lg p-6"
      >
        <h2
          id={titleId}
          className="font-display text-base font-semibold text-text"
        >
          {title}
        </h2>
        <div className="mt-2 text-sm text-text-muted">{description}</div>
        {details && details.length > 0 && (
          <dl className="mt-4 space-y-1.5 font-mono text-xs">
            {details.map((row) => (
              <div key={row.label} className="flex justify-between gap-4">
                <dt className="text-text-subtle">{row.label}</dt>
                <dd className="text-text">{row.value}</dd>
              </div>
            ))}
          </dl>
        )}
        <div className="mt-6 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            disabled={submitting}
            className="rounded-[var(--button-radius)] px-3 py-1.5 font-mono text-xs text-text-muted transition-colors hover:bg-surface-muted disabled:opacity-50"
          >
            {cancelLabel}
          </button>
          <button
            ref={confirmRef}
            type="button"
            onClick={onConfirm}
            disabled={submitting}
            className={`${confirmClasses} rounded-[var(--button-radius)] px-3 py-1.5 font-mono text-xs font-medium transition-opacity hover:opacity-90 disabled:opacity-50`}
          >
            {submitting && confirmingLabel ? confirmingLabel : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}