// Ported from Fibration Lab apps/desktop/src/ui/Toasts.tsx at commit f229e4d.
import { useToasts } from "../toasts";

export function Toasts() {
  const toasts = useToasts((s) => s.toasts);
  if (toasts.length === 0) return null;
  return (
    <div className="toasts" aria-live="assertive">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`} role={t.kind === "error" ? "alert" : "status"}>
          <span className="grow">{t.text}</span>
          <button type="button" className="ghost small" onClick={() => useToasts.getState().dismissToast(t.id)} aria-label="Dismiss">
            ×
          </button>
        </div>
      ))}
    </div>
  );
}
