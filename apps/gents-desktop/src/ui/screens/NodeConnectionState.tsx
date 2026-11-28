import { useEffect, useState } from "react";
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

/** How long a page must stay out of date before it says so. */
export const STALE_NOTICE_AFTER_MS = 3_000;

export function StaleNotice({
  state,
  className,
}: {
  state: PageConnection;
  className?: string;
}) {
  /* a connection that is only catching up (a launch drawn from the cache
     before the node answers) passes unannounced: the line shows once the
     page has been out of date this long, so it does not push the page down
     and back up in the first second */
  const [due, setDue] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setDue(true), STALE_NOTICE_AFTER_MS);
    return () => clearTimeout(timer);
  }, []);
  if (!due) return null;
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
