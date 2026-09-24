/* What a person can do to a running worker from its parent: stop it. That
   is the desktop's interrupt, previewed as a cascade first, the way Stop
   is. Opening the worker is the row's own arrow, so the row carries one
   control and no menu. */
import { createContext, useContext } from "react";
import { Square } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Hint } from "./Hint";

export type WorkerActions = {
  /* the parent's request, the one a worker action is recorded under */
  parentRequestId: string | null;
  /* the child's current request, so the cascade preview and interrupt hit the right one */
  cancel: (requestId: string) => void;
};

export const WorkerActionsContext = createContext<WorkerActions | null>(null);

export function WorkerStop({
  name,
  /* the request the child is on now, which may be later than the spawn's */
  currentRequestId,
  running,
}: {
  name: string;
  currentRequestId: string | null;
  running: boolean;
}) {
  const actions = useContext(WorkerActionsContext);
  /* nothing to stop once it has settled: the row keeps its arrow alone */
  if (!actions || !running || !currentRequestId) return null;
  return (
    <Hint label={`Stop ${name}`}>
      <Button
        variant="quiet"
        size="icon-xs"
        aria-label={`Stop ${name}`}
        onClick={() => actions.cancel(currentRequestId)}
      >
        <Square className="size-3.5 fill-current" />
      </Button>
    </Hint>
  );
}
