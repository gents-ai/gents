import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import type { PageConnection } from "@/lib/nodeConnection";

/** In place of a page's empty state while its node connects or can't be
    reached. */
export function ConnectionPlaceholder({
  state,
  className,
}: {
  state: Extract<PageConnection, { kind: "loading" | "unavailable" }>;
  className?: string;
}) {
  return (
    <div
      role="status"
      data-testid="node-connection-state"
      className={cn("grid place-items-center py-10 text-center", className)}
    >
      {state.kind === "loading" ? (
        <p className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner className="text-foreground" />
          {state.title}
        </p>
      ) : (
        <div className="max-w-sm">
          <p className="font-heading text-base font-medium text-heading">
            {state.title}
          </p>
          <p className="mt-1 text-sm text-muted-foreground">{state.detail}</p>
        </div>
      )}
    </div>
  );
}

/** One line over data the node can't currently refresh. */
export function StaleNotice({ text, className }: { text: string; className?: string }) {
  return (
    <p
      role="status"
      data-testid="node-connection-stale"
      className={cn("text-xs text-muted-foreground", className)}
    >
      {text}
    </p>
  );
}
