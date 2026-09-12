/* A session's state as a 16px glyph in front of its title: a hollow dot
   for a finished one, the braille spinner while it streams, a clock while
   it waits for an agent, and a filled brand dot when a held tool call
   needs a person. Failures and interruptions get their own Lucide mark. */
import { CircleMinus, CircleX, Clock } from "lucide-react";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { turnLabel } from "@/lib/live";

export function SessionStatus({
  turnState,
  held = false,
  className,
}: {
  turnState: string | null | undefined;
  held?: boolean;
  className?: string;
}) {
  const label = held ? "Needs you" : (turnLabel(turnState) ?? "Finished");
  const glyph = held ? (
    <span className="size-2 rounded-full bg-brand" />
  ) : turnState === "streaming" ? (
    <Spinner className="text-foreground" />
  ) : turnState === "waitingForClaim" ? (
    <Clock className="size-3.5 text-muted-foreground" />
  ) : turnState === "failed" ? (
    <CircleX className="size-3.5 text-destructive" />
  ) : turnState === "interrupted" || turnState === "superseded" ? (
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
