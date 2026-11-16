import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { connectionLine, type PageConnection } from "@/lib/nodeConnection";

export function ConnectionPlaceholder({
  state,
  className,
}: {
  state: PageConnection;
  className?: string;
}) {
  const line = connectionLine(state.name, state.status);
  return (
    <div
      role="status"
      data-testid="node-connection-state"
      className={cn("grid place-items-center py-10 text-center", className)}
    >
      {state.kind === "loading" ? (
        <p className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner className="text-foreground" />
          {`${line}…`}
        </p>
      ) : (
        <div className="max-w-sm">
          <p className="font-heading text-base font-medium text-heading">{line}</p>
          <p className="mt-1 text-sm text-muted-foreground">{state.status.detail}</p>
        </div>
      )}
    </div>
  );
}

export function StaleNotice({
  state,
  className,
}: {
  state: PageConnection;
  className?: string;
}) {
  return (
    <p
      role="status"
      data-testid="node-connection-stale"
      className={cn("text-xs text-muted-foreground", className)}
    >
      {`${connectionLine(state.name, state.status)}. This may be out of date.`}
    </p>
  );
}
