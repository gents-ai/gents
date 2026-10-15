/* A session's state as a 16px glyph in front of its title: a hollow dot
   for a finished one, the braille spinner while it streams, and a clock
   while it waits for an agent. Failures and interruptions get their own
   Lucide mark. */
import { CircleMinus, CircleX, Clock } from "lucide-react";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { stateOf, turnLabel } from "@/lib/live";

export function SessionStatus({
  turnState,
  className,
}: {
  turnState: string | null | undefined;
  className?: string;
}) {
  const label = turnLabel(turnState) ?? "Finished";
  const state = stateOf(turnState);
  const glyph =
    state === "waiting" ? (
      <Clock className="size-3.5 text-muted-foreground" />
    ) : state === "working" ? (
      <Spinner className="text-foreground" />
    ) : state === "failed" ? (
      <CircleX className="size-3.5 text-destructive" />
    ) : state === "stopped" ? (
      <CircleMinus className="size-3.5 text-muted-foreground" />
    ) : (
      <span className="size-2 rounded-full border border-muted-foreground/60" />
    );
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className={cn("grid size-4 shrink-0 place-items-center", className)}
    >
      {glyph}
    </span>
  );
}
