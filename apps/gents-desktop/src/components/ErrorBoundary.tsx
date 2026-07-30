import { Component, type ErrorInfo, type ReactNode } from "react";

/// Last-resort boundary: a render exception anywhere below used to
/// white-screen the whole desktop app. Class component by necessity —
/// React only exposes error boundaries via lifecycle methods.
type ErrorBoundaryProps = { children: ReactNode };
type ErrorBoundaryState = { error: Error | null; componentStack: string | null };

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null, componentStack: null };

  static getDerivedStateFromError(error: Error): Partial<ErrorBoundaryState> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("desktop render error", error, info.componentStack);
    this.setState({ componentStack: info.componentStack ?? null });
  }

  render() {
    if (!this.state.error) {
      return this.props.children;
    }
    const message = this.state.error.message || String(this.state.error);
    return (
      <main className="app-shell">
        <article
          className="panel centered-panel error-boundary"
          data-testid="error-boundary"
          role="alert"
        >
          <p className="eyebrow">Desktop</p>
          <h2>Something went wrong</h2>
          <p className="muted">
            The view hit an unexpected error. Reloading usually recovers; your agents
            and data are unaffected.
          </p>
          <details className="error-boundary-details">
            <summary>Error details</summary>
            <pre>
              {this.state.error.stack || message}
              {this.state.componentStack
                ? `\n\nComponent stack:${this.state.componentStack}`
                : null}
            </pre>
          </details>
          <button
            autoFocus
            className="primary-button"
            data-testid="error-boundary-reload"
            type="button"
            onClick={() => window.location.reload()}
          >
            Reload
          </button>
        </article>
      </main>
    );
  }
}
