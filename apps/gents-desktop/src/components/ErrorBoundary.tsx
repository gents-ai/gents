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
      <main className="viewport-frame grid place-items-center bg-background px-8 text-foreground">
        <article
          className="grid w-full max-w-md gap-4"
          data-testid="error-boundary"
          role="alert"
        >
          <p className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            Desktop
          </p>
          <h2 className="font-heading text-2xl font-medium text-heading">
            Something went wrong
          </h2>
          <p className="text-sm text-muted-foreground">
            The view hit an unexpected error. Reloading usually recovers; your agents
            and data are unaffected.
          </p>
          <details>
            <summary>Error details</summary>
            <pre className="mt-2 overflow-auto font-mono text-xs">
              {this.state.error.stack || message}
              {this.state.componentStack
                ? `\n\nComponent stack:${this.state.componentStack}`
                : null}
            </pre>
          </details>
          <button
            autoFocus
            className="inline-flex h-8 w-fit items-center rounded-lg bg-brand px-3 text-sm font-medium text-brand-foreground"
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
