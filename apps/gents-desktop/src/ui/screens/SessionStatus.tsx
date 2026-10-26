import { CircleMinus, CircleX, Clock } from "lucide-react";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { stateOf, turnLabel } from "@/lib/live";
import { useIsNew } from "@/lib/seen";

/** A session's state, and, where its work finished since it was last on
    screen here, that it is new: a filled ring when finished, a dot on the
    corner of a failed or stopped mark. */
export function SessionStatus({
  turnState,
  fresh = false,
  className,
}: {
  turnState: string | null | undefined;
  fresh?: boolean;
  className?: string;
}) {
  const state = stateOf(turnState);
  const label =
    (turnLabel(turnState) ?? "Finished") + (fresh ? " · new since you looked" : "");
  const glyph =
    state === "waiting" ? (
      <Clock className="size-3.5 text-muted-foreground" />
    ) : state === "working" ? (
      <Spinner className="text-foreground" />
    ) : state === "failed" ? (
      <CircleX className="size-3.5 text-destructive" />
    ) : state === "stopped" ? (
      <CircleMinus className="size-3.5 text-muted-foreground" />
    ) : fresh ? (
      <span className="size-2 rounded-full bg-brand" />
    ) : (
      <span className="size-2 rounded-full border border-muted-foreground/60" />
    );
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className={cn("relative grid size-4 shrink-0 place-items-center", className)}
    >
      {glyph}
      {fresh && (state === "failed" || state === "stopped") && (
        <span className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-brand ring-2 ring-background" />
      )}
    </span>
  );
}

/** `SessionStatus` for a listed session, marked new from what was seen. */
export function ListedSessionStatus({
  session,
  className,
}: {
  session: SessionSummary;
  className?: string;
}) {
  return (
    <SessionStatus
      turnState={session.turnState}
      fresh={useIsNew(session)}
      className={className}
    />
  );
}
