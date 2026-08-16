import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { api } from "../api";
import { qk } from "../queries";

/** Parse the episode id from the server's `Merged as episode: <id>` /
 *  `Ingested as episode: <id>` confirmation string. Returns `null` if the
 *  text doesn't match the well-known shape. */
function parseEpisodeId(text: string): string | null {
  const m = /Ingested as episode:\s*([^\s.]+)/.exec(text);
  return m ? m[1] : null;
}

const SPACE = "personal";

export function CaptureView() {
  const queryClient = useQueryClient();
  const [text, setText] = useState("");
  const [lastServerText, setLastServerText] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: (content: string) => api.remember(content, SPACE),
    onSuccess: (serverText) => {
      toast.success(serverText || "Captured");
      setLastServerText(serverText);
      setText("");
      // Sidebar space count refreshes via qk.space invalidation.
      void queryClient.invalidateQueries({ queryKey: qk.space });
    },
    onError: (e: unknown) => {
      toast.error(e instanceof Error ? e.message : String(e));
    },
  });

  // Auto-focus the textarea whenever the form is (re)shown — mount and
  // after "capture another". Keyed on form visibility because the textarea
  // unmounts while the success card is up (ref is null synchronously in
  // the click handler, so focusing there would no-op).
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    if (!lastServerText) {
      textareaRef.current?.focus();
    }
  }, [lastServerText]);

  const canSubmit = !mutation.isPending && text.trim().length > 0;

  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col p-8">
      <header className="mb-6">
        <h1 className="font-display text-2xl font-semibold text-text">
          Capture
        </h1>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          record a thought — episodes are immutable. Extraction runs immediately
          if a model is available.
        </p>
      </header>

      {/* If we have a server confirmation from the latest submit, replace the
          form area with a success card so the user can copy the episode id
          and confirm the result before composing the next note. */}
      {lastServerText ? (
        <SuccessCard
          serverText={lastServerText}
          onCaptureAnother={() => {
            setLastServerText(null);
          }}
        />
      ) : (
        <>
          <textarea
            ref={textareaRef}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                if (canSubmit) mutation.mutate(text.trim());
              }
            }}
            placeholder="What happened? What did you learn? Who did you talk to?"
            className="min-h-[10rem] flex-1 resize-none rounded-[var(--input-radius)] bg-surface px-3.5 py-2.5 text-sm text-text placeholder:text-text-subtle shadow-[var(--input-shadow)] focus-visible:shadow-[var(--input-shadow-focus)] focus-visible:outline-none"
          />

          <div className="mt-4 flex items-center justify-between">
            <span className="font-mono text-xs text-text-subtle">
              ⌘↵ to submit · space: {SPACE}
            </span>
            <button
              type="button"
              onClick={() => mutation.mutate(text.trim())}
              disabled={!canSubmit}
              className="bg-interactive-primary text-interactive-primary-foreground rounded-[var(--button-radius)] px-4 py-2 font-mono text-xs font-medium transition-opacity hover:opacity-90 disabled:opacity-40"
            >
              {mutation.isPending ? "capturing…" : "capture"}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

interface SuccessCardProps {
  serverText: string;
  onCaptureAnother: () => void;
}

function SuccessCard({ serverText, onCaptureAnother }: SuccessCardProps) {
  const episodeId = parseEpisodeId(serverText);
  return (
    <section
      aria-label="Capture result"
      className="animate-fade-in rounded-[var(--card-radius)] border border-line bg-surface-raised p-5"
    >
      <div className="flex items-center gap-2">
        <span className="text-status-success">✓</span>
        <span className="font-display text-sm font-medium text-text">
          Captured
        </span>
      </div>
      {episodeId && (
        <p className="mt-3 font-mono text-xs text-text-subtle">episode</p>
      )}
      {episodeId && (
        <p
          className="mt-1 break-all font-mono text-sm text-text"
          title={episodeId}
        >
          {episodeId}
        </p>
      )}
      <p className="mt-3 font-mono text-xs text-text-subtle">server response</p>
      <p className="mt-1 whitespace-pre-wrap break-words font-mono text-xs text-text-subtle">
        {serverText}
      </p>
      <div className="mt-5 flex justify-end">
        <button
          type="button"
          onClick={onCaptureAnother}
          className="bg-interactive-primary text-interactive-primary-foreground rounded-[var(--button-radius)] px-4 py-2 font-mono text-xs font-medium transition-opacity hover:opacity-90"
        >
          capture another
        </button>
      </div>
    </section>
  );
}