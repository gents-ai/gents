// Native background processes have no desktop stop control.
import { createContext, useContext } from "react";
import { Square } from "lucide-react";
import type { CausedRequestView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Hint } from "./Hint";
import { requestIsLive } from "./workers";

export type WorkerActions = {
  interrupt: (request: CausedRequestView) => void;
};

export const WorkerActionsContext = createContext<WorkerActions | null>(null);

export function StopButton({ name, onStop }: { name: string; onStop: () => void }) {
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

export function RequestStop({
  name,
  request,
}: {
  name: string;
  request: CausedRequestView | null;
}) {
  const actions = useContext(WorkerActionsContext);
  if (!actions || !request || !requestIsLive(request.lifecycleState)) return null;
  return <StopButton name={name} onStop={() => actions.interrupt(request)} />;
}
