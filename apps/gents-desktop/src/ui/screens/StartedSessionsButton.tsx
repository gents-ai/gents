import { SquareStack } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { dockScopeOf, useDockFor } from "@/app/workspace";
import { isLive } from "@/lib/live";
import { Hint } from "@/screens/Hint";
import { nodeOf } from "../../hooks/fleetStore";
import { useFleet, workersOfId } from "../hooks/useFleet";
import { agentName } from "./behavior";
import { AgentInitials } from "./parts";

const SHOWN_AGENTS = 3;

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
  /* each agent once, by node and agent, in the order its first session started */
  const agents = useFleet(
    useShallow((state) => {
      const seen = new Map<string, string>();
      for (const w of workersOfId(state, sessionId)) {
        const key = `${w.nodeDid}\u0000${w.agentId ?? ""}`;
        if (!seen.has(key))
          seen.set(key, agentName(w.agentId, nodeOf(state, w.nodeDid)));
      }
      return [...seen.values()];
    }),
  );
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
          <span className="flex items-center">
            {agents.slice(0, SHOWN_AGENTS).map((name, i) => (
              <AgentInitials
                key={i}
                name={name}
                className="size-5 text-[9px] ring-2 ring-background not-first:-ml-1.5"
              />
            ))}
          </span>
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
