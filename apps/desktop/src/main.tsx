import "dockview-react/dist/styles/dockview.css";
import "./styles/theme.css";
import "./styles/app.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { ErrorBoundary } from "./ui/common/ErrorBoundary";

declare global {
  interface Window {
    /** Uncaught errors and unhandled rejections since load; the smoke test asserts this stays empty. */
    __analystErrors: string[];
  }
}

function describe(reason: unknown): string {
  if (reason instanceof Error) return reason.stack ?? reason.message;
  if (typeof reason === "object" && reason !== null && "message" in reason) return String((reason as { message: unknown }).message);
  return String(reason);
}

window.__analystErrors = [];
window.addEventListener("error", (e) => {
  // ResizeObserver loop notifications are benign browser noise, not app errors.
  if (typeof e.message === "string" && e.message.startsWith("ResizeObserver loop")) return;
  window.__analystErrors.push(e.error ? describe(e.error) : e.message);
});
window.addEventListener("unhandledrejection", (e) => {
  window.__analystErrors.push(`unhandled rejection: ${describe(e.reason)}`);
});

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");
createRoot(root).render(
  <StrictMode>
    <ErrorBoundary label="Personal Analyst">
      <App />
    </ErrorBoundary>
  </StrictMode>,
);
