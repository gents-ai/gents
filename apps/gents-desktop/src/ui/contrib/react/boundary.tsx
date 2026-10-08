/* The blast wall between a contribution's render() and the app. A plugin
   throwing during render degrades to a small inline error in its own slot;
   the surface around it keeps working. Class component by necessity: React
   only exposes error boundaries through lifecycle methods. After
   hermes-agent's contrib/react/boundary.tsx. */
import { Component, createElement, type ErrorInfo, type ReactNode } from "react";
import { TriangleAlert } from "lucide-react";

interface ContribBoundaryProps {
  children: ReactNode;
  /** the contribution key, shown in the fallback and the console tag */
  id: string;
  /** `chip` is an inline bar item; `pane` fills a page */
  variant?: "chip" | "pane";
}

interface ContribBoundaryState {
  error: Error | null;
}

export class ContribBoundary extends Component<
  ContribBoundaryProps,
  ContribBoundaryState
> {
  state: ContribBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ContribBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(
      `[ux-plugins] contrib:${this.props.id} failed to render`,
      error,
      info.componentStack,
    );
  }

  reset = () => this.setState({ error: null });

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const { id, variant = "pane" } = this.props;
    if (variant === "chip") {
      return (
        <button
          type="button"
          className="inline-flex items-center gap-1 rounded px-1.5 text-[11px] text-destructive"
          title={`${id}: ${error.message}`}
          onClick={this.reset}
          data-testid="contrib-error-chip"
        >
          <TriangleAlert className="size-3" />
          {id}
        </button>
      );
    }
    return (
      <div
        className="grid gap-2 rounded-lg border border-border p-4"
        data-testid="contrib-error-pane"
      >
        <p className="flex items-center gap-2 text-sm font-medium text-foreground">
          <TriangleAlert className="size-4 text-destructive" />
          {`“${id}” failed to render`}
        </p>
        <p className="font-mono text-xs text-muted-foreground">{error.message}</p>
        <button
          type="button"
          className="w-fit rounded-md border border-border px-2 py-1 text-xs"
          onClick={this.reset}
        >
          Retry
        </button>
      </div>
    );
  }
}

/* mount a render callback as a component so its hooks and errors belong to
   the contribution, not to whichever surface happened to call it */
export function ContribRender({ render }: { render: () => ReactNode }) {
  return createElement(render);
}
