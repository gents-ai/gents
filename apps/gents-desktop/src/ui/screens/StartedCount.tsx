import { SquareStack } from "lucide-react";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { href } from "@/lib/router";
import { sessionKeyOf } from "../../hooks/fleetStore";

const TRIGGER =
  "relative z-10 -mx-1.5 flex items-center gap-1 rounded-md px-1.5 py-1 text-xs text-muted-foreground tabular-nums hover:bg-foreground/6 hover:text-foreground aria-expanded:text-foreground";

/** How many sessions `session` started, with a dot while any of them, or
    anything they started, is working. A link to that level of the list, or,
    given `onToggle`, a disclosure for showing them in place. */
export function StartedCount({
  session,
  started,
  running,
  open,
  onToggle,
}: {
  session: SessionSummary;
  started: number;
  running: number;
  open?: boolean;
  onToggle?: () => void;
}) {
  if (started === 0) return null;
  const count =
    `${started} started session${started === 1 ? "" : "s"}` +
    (running > 0 ? ` · ${running} running` : "");
  return (
    <Tooltip disableHoverablePopup>
      <TooltipTrigger
        render={
          onToggle ? (
            <button
              type="button"
              onClick={onToggle}
              aria-expanded={open ?? false}
              aria-label={`${open ? "Hide" : "Show"} ${count}`}
              className={TRIGGER}
            />
          ) : (
            <a
              href={href({ name: "sessionsUnder", session: sessionKeyOf(session) })}
              data-testid={`sessions-under-${session.sessionId}`}
              aria-label={`Explore ${count}`}
              className={TRIGGER}
            />
          )
        }
      >
        <SquareStack className="size-3.5" />
        {started}
        {running > 0 && (
          <span className="absolute top-0.5 left-4 size-1.5 rounded-full bg-brand" />
        )}
      </TooltipTrigger>
      <TooltipContent side="top" className="pointer-events-none">
        {onToggle
          ? `${open ? "Hide" : "Show"} started sessions`
          : "Explore started sessions"}
      </TooltipContent>
    </Tooltip>
  );
}
