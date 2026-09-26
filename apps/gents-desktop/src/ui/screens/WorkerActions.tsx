/* What a person can do to a running subagent from the session that reached
   it: stop the one request it is working on, the same interrupt as that
   session's own Stop. Nothing else stops with it, and nothing this session
   is doing stops. Telling it something is sending that session a message,
   so it is done there; opening it is the row's own arrow. */
import { createContext, useContext } from "react";
import { Square } from "lucide-react";
import type { CausedRequestView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Hint } from "./Hint";

export type WorkerActions = {
  stop: (request: CausedRequestView) => void;
};

export const WorkerActionsContext = createContext<WorkerActions | null>(null);

export function WorkerStop({
  name,
  /* the subagent's running request, which may be later than the call's own */
  live,
}: {
  name: string;
  live: CausedRequestView | null;
}) {
  const actions = useContext(WorkerActionsContext);
  /* nothing to stop once it has settled: the row keeps its arrow alone */
  if (!actions || !live) return null;
  return (
    <Hint label={`Stop ${name}`}>
      <Button
        variant="quiet"
        size="icon-xs"
        aria-label={`Stop ${name}`}
        onClick={() => actions.stop(live)}
      >
        <Square className="size-3.5 fill-current" />
      </Button>
    </Hint>
  );
}
