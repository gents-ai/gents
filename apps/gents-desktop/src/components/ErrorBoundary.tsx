import { Component, type ErrorInfo, type ReactNode } from "react";

import { isWindowsTauriShell } from "../lib/shellPlatform";
import { WindowControls } from "../ui/app/WindowControls";
import { Disclosure } from "../ui/components/Disclosure";
import { useCopied } from "../ui/lib/clipboard";

/* the details in their own scrolling box, with a way to copy them whole
   for a report */
function ErrorDetails({ text }: { text: string }) {
  const { copied, copy } = useCopied();
  return (
    <div className="relative mt-2">
      <pre
        className="max-h-[40vh] overflow-auto rounded-lg bg-raised/50 p-3 pr-16 font-mono text-xs [scrollbar-color:var(--color-border)_transparent] [scrollbar-width:thin]"
        tabIndex={0}
      >
        {text}
      </pre>
      <button
        type="button"
        onClick={() => copy(text)}
        className="absolute top-2 right-3 rounded-md bg-background/80 px-2 py-1 text-xs text-muted-foreground hover:text-foreground"
      >
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

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
    const stack = this.state.error.stack ?? "";
    // WebKit's stack lists frames only, without the error's message.
    const trace = stack.includes(message) ? stack : `${message}\n\n${stack}`;
    return (
      <>
        <div className="titlebar-drag-region" data-tauri-drag-region>
          {isWindowsTauriShell() && <WindowControls />}
        </div>
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
            <p
              className="font-mono text-xs break-words text-foreground"
              data-testid="error-boundary-message"
            >
              {message}
            </p>
            <Disclosure summary="Error details" className="min-w-0">
              <ErrorDetails
                text={`${trace.trim()}${
                  this.state.componentStack
                    ? `\n\nComponent stack:${this.state.componentStack}`
                    : ""
                }`}
              />
            </Disclosure>
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
      </>
    );
  }
}
