/* What a person can do to a worker from the session that started it:
   stop the one request a row's call caused, the modeled single-request
   interrupt; the row settles when that request reaches a terminal state.
   Nothing else stops with it, and nothing this session is doing stops.
   Telling a worker something is sending its session a message, so it is
   done there; opening it is the row's arrow. A native background process
   has no desktop stop (#1969). */
import { createContext, useContext } from "react";
import { Square } from "lucide-react";
import type { CausedRequestView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Hint } from "./Hint";
import { isLive } from "@/lib/live";

export type WorkerActions = {
  /* interrupt the request a session-message row's call caused */
  interrupt: (request: CausedRequestView) => void;
};

export const WorkerActionsContext = createContext<WorkerActions | null>(null);

function StopButton({ name, onStop }: { name: string; onStop: () => void }) {
  return (
    <Hint label={`Stop ${name}`}>
      <Button
        variant="quiet"
        size="icon-xs"
        aria-label={`Stop ${name}`}
        onClick={onStop}
      >
        <Square className="size-3.5 fill-current" />
      </Button>
    </Hint>
  );
}

/* Stop on a session-message row, while the request its call caused runs. */
export function RequestStop({
  name,
  request,
}: {
  name: string;
  request: CausedRequestView | null;
}) {
  const actions = useContext(WorkerActionsContext);
  if (!actions || !request || !isLive(request.lifecycleState)) return null;
  return <StopButton name={name} onStop={() => actions.interrupt(request)} />;
}
