import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";

interface Props {
  /** Shown in the fallback, e.g. the panel name. */
  label: string;
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/** Keeps a render error inside one panel (or, at the root, on a readable page) instead of blanking the app. */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: unknown): State {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    console.error(`[${this.props.label}]`, error, info.componentStack);
  }

  render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="empty error-boundary" role="alert" data-testid="error-boundary">
        <p>
          <strong>{this.props.label}</strong> failed to render.
        </p>
        <pre className="small">{error.message}</pre>
        <button type="button" className="small" onClick={() => this.setState({ error: null })}>
          Try again
        </button>
      </div>
    );
  }
}
