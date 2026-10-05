/* The work this session handed out: every session it spawned, on any
   node, with its state and a way in. The same lineage the sessions list
   folds under a parent, shown flat beside the parent. */
import type { SurfaceContext } from "@/app/surfaces";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { href } from "@/lib/router";
import { nodeOfSession } from "@/lib/nodes";
import { NodeBehaviorStack } from "./NodeBehaviorStack";
import { SessionStatus } from "./SessionStatus";
import { when } from "./time";
import { useFleet, workersOfId } from "../hooks/useFleet";
import { useDeployments, useHomeDid, useSelectedDeployment } from "@/hooks/useClient";

export function WorkersSurface({ sessionId }: SurfaceContext) {
  const deployments = useDeployments();
  const selectedDeployment = useSelectedDeployment();
  const homeDid = useHomeDid();
  const workers = useFleet((s) => workersOfId(s, sessionId));
  return (
    <ScrollArea className="h-full">
      <div className="px-3 py-2">
        {workers.length === 0 && (
          <p className="py-6 text-center text-sm text-muted-foreground">
            This session has not handed out any work.
          </p>
        )}
        <ul>
          {workers.map((w) => (
            <li key={w.sessionId}>
              <a
                href={href({ name: "session", sessionId: w.sessionId })}
                className="-mx-1 grid grid-cols-[auto_1fr_auto_auto] items-center gap-3 rounded-lg px-2 py-2 text-sm hover:bg-accent"
              >
                <SessionStatus turnState={w.turnState} />
                <span className="truncate">{w.title ?? "Untitled"}</span>
                <NodeBehaviorStack
                  nodes={deployments}
                  homeDid={homeDid}
                  nodeDid={nodeOfSession(w)}
                  behaviorId={w.behaviorId}
                  deployment={selectedDeployment}
                  size="sm"
                />
                <span className="w-8 text-right text-xs text-muted-foreground">
                  {when(w.updatedAt)}
                </span>
              </a>
            </li>
          ))}
        </ul>
      </div>
    </ScrollArea>
  );
}
