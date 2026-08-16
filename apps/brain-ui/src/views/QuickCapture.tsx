import { useState, useCallback } from "react";
import { api } from "../api";

export function QuickCapture() {
  const [text, setText] = useState("");
  const [space, setSpace] = useState("personal");
  const [result, setResult] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = useCallback(async () => {
    if (!text.trim()) return;
    setLoading(true);
    setError(null);
    setResult(null);
    try {
      const r = await api.remember(text.trim(), space);
      setResult(r);
      setText("");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to capture");
    }
    setLoading(false);
  }, [text, space]);

  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col p-8">
      <div className="mb-6">
        <h3 className="font-display text-xl text-text">Capture a thought</h3>
        <p className="mt-1 font-mono text-xs text-text-faint">
          Episodes are immutable. Extraction runs immediately if a model is available.
        </p>
      </div>

      {/* Text area */}
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") submit();
        }}
        placeholder="What happened? What did you learn? Who did you talk to?"
        autoFocus
        className="min-h-[200px] flex-1 resize-none rounded-xl border border-line bg-surface p-4 text-text placeholder:text-text-faint focus:border-amber focus:outline-none"
      />

      {/* Controls */}
      <div className="mt-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="font-mono text-xs text-text-faint">space:</span>
          <input
            value={space}
            onChange={(e) => setSpace(e.target.value)}
            className="w-28 rounded-md border border-line bg-surface px-2 py-1 font-mono text-xs text-text-dim focus:border-amber focus:outline-none"
          />
        </div>
        <div className="flex items-center gap-3">
          <span className="font-mono text-xs text-text-faint">
            ⌘↵ to submit
          </span>
          <button
            onClick={submit}
            disabled={loading || !text.trim()}
            className="rounded-xl bg-amber px-6 py-2 font-medium text-ink transition-opacity hover:opacity-90 disabled:opacity-40"
          >
            {loading ? "Capturing…" : "Capture"}
          </button>
        </div>
      </div>

      {/* Result */}
      {result && (
        <div className="mt-4 rounded-xl border border-sage/30 bg-sage/5 p-4 animate-fade-in">
          <div className="flex items-center gap-2">
            <span className="text-sage">✓</span>
            <span className="text-sm text-text">Episode captured</span>
          </div>
          <p className="mt-2 whitespace-pre-wrap font-mono text-xs text-text-dim">
            {result}
          </p>
        </div>
      )}

      {error && (
        <div className="mt-4 rounded-xl border border-rose/30 bg-rose/5 p-4">
          <p className="font-mono text-sm text-rose">{error}</p>
        </div>
      )}
    </div>
  );
}
