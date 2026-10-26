import { SquareStack } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { dockScopeOf, useDockFor } from "@/app/workspace";
import { isLive } from "@/lib/live";
import { Hint } from "@/screens/Hint";
import { useFleet, workersOfId } from "../hooks/useFleet";
import { useWorkerAgents, WorkerAgents } from "./WorkerAgents";

const SURFACE = "workers";

export function StartedSessionsButton({
  sessionId,
  className,
}: {
  sessionId: string;
  className?: string;
}) {
  const { started, running } = useFleet(
    useShallow((state) => {
      const workers = workersOfId(state, sessionId);
      return {
        started: workers.length,
        running: workers.filter((w) => isLive(w.turnState)).length,
      };
    }),
  );
  const agents = useWorkerAgents((state) => workersOfId(state, sessionId));
  const { dock, openSurface, closeTab } = useDockFor(dockScopeOf("session", sessionId));
  if (started === 0) return null;
  const showing = dock.open && dock.active === SURFACE;
  const label =
    `${started} started session${started === 1 ? "" : "s"}` +
    (running > 0 ? `, ${running} running` : "");
  return (
    <Hint label={label}>
      <Button
        variant="ghost"
        size="sm"
        aria-label={label}
        aria-pressed={showing}
        className={cn(
          "relative h-7 shrink-0 gap-1 px-2 text-xs text-muted-foreground tabular-nums",
          showing && "bg-accent text-foreground",
          className,
        )}
        onClick={() => (showing ? closeTab(SURFACE) : openSurface(SURFACE))}
      >
        {agents.length > 0 ? (
          <WorkerAgents names={agents} />
        ) : (
          <SquareStack className="size-3.5" />
        )}
        {started}
        {running > 0 && (
          <span
            aria-hidden="true"
            data-testid="started-sessions-running"
            className="absolute top-1 right-1 size-1.5 rounded-full bg-brand motion-safe:animate-pulse"
          />
        )}
      </Button>
    </Hint>
  );
}
