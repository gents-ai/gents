import { SquareStack } from "lucide-react";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { sessionKeyOf } from "../../hooks/fleetStore";
import { workersOf } from "../hooks/useFleet";
import { useWorkerAgents, WorkerAgents } from "./WorkerAgents";

const TRIGGER =
  "relative z-10 -mx-1.5 flex items-center gap-1 rounded-md px-1.5 py-1 text-xs text-muted-foreground tabular-nums hover:bg-foreground/6 hover:text-foreground aria-expanded:text-foreground";

/** What `started`, `running`, `failed` and `stopped` say together, for a
    label. */
export const startedCount = ({
  started,
  running,
  failed = 0,
  stopped = 0,
}: {
  started: number;
  running: number;
  failed?: number;
  stopped?: number;
}) =>
  `${started} started session${started === 1 ? "" : "s"}` +
  (running > 0 ? ` · ${running} running` : "") +
  (failed > 0 ? ` · ${failed} failed` : "") +
  (stopped > 0 ? ` · ${stopped} stopped` : "");

/** How many sessions `session` started, after their agents, with a dot
    while any of them, or anything they started, is working, and a red one
    when any of them failed, which matters more. A link to that level of the
    list, or, given `onToggle`, a disclosure for showing them in place. */
export function StartedCount({
  session,
  started,
  running,
  failed = 0,
  stopped = 0,
  open,
  onToggle,
}: {
  session: SessionSummary;
  started: number;
  running: number;
  failed?: number;
  stopped?: number;
  open?: boolean;
  onToggle?: () => void;
}) {
  const agents = useWorkerAgents((state) => workersOf(state, session));
  if (started === 0) return null;
  const count = startedCount({ started, running, failed, stopped });
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
        {agents.length > 0 ? (
          <WorkerAgents names={agents} />
        ) : (
          <SquareStack className="size-3.5" />
        )}
        {started}
        {(failed > 0 || running > 0) && (
          <span
            data-testid="started-mark"
            className={cn(
              "absolute top-0.5 right-0.5 size-1.5 rounded-full",
              failed > 0 ? "bg-destructive" : "bg-brand",
            )}
          />
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
