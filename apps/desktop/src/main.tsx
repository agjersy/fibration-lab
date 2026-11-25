import "dockview-react/dist/styles/dockview.css";
import "./styles/theme.css";
import "./styles/app.css";
import "./styles/slide.css";
import "./styles/deck.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { ErrorBoundary } from "./ui/common/ErrorBoundary";

declare global {
  interface Window {
    /** Uncaught errors and unhandled rejections since load; the smoke test asserts this stays empty. */
    __fibrationErrors: string[];
  }
}

function describe(reason: unknown): string {
  if (reason instanceof Error) return reason.stack ?? reason.message;
  if (typeof reason === "object" && reason !== null && "message" in reason) return String((reason as { message: unknown }).message);
  return String(reason);
}

window.__fibrationErrors = [];
window.addEventListener("error", (e) => {
  // ResizeObserver loop notifications are benign browser noise, not app errors.
  if (typeof e.message === "string" && e.message.startsWith("ResizeObserver loop")) return;
  window.__fibrationErrors.push(e.error ? describe(e.error) : e.message);
});
window.addEventListener("unhandledrejection", (e) => {
  window.__fibrationErrors.push(`unhandled rejection: ${describe(e.reason)}`);
});

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");
createRoot(root).render(
  <StrictMode>
    <ErrorBoundary label="Fibration Lab">
      <App />
    </ErrorBoundary>
  </StrictMode>,
);
