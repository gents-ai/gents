import { CircleMinus, CircleX, Clock } from "lucide-react";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { isLive, turnLabel } from "@/lib/live";

export function SessionStatus({
  turnState,
  className,
}: {
  turnState: string | null | undefined;
  className?: string;
}) {
  const label = turnLabel(turnState) ?? "Finished";
  const glyph =
    turnState === "waitingForClaim" ? (
      <Clock className="size-3.5 text-muted-foreground" />
    ) : isLive(turnState) ? (
      <Spinner className="text-foreground" />
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
