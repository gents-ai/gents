import { useShallow } from "zustand/react/shallow";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../hooks/fleetStore";
import { cn } from "@gents/ui/lib/utils";
import { useHomeDid } from "@/hooks/useClient";
import { nodeOf } from "../../hooks/fleetStore";
import { useFleet } from "@/hooks/useFleet";
import { isWorkingNode, nodeDidOf } from "@/lib/nodes";
import { NodeAvatar } from "./AgentAvatar";
import { AgentHoverCard, NodeHoverCard } from "./HoverCards";
import { AgentInitials } from "./parts";
import { agentName } from "./behavior";

export function NodeAgentStack({
  nodeDid,
  agentId,
  description,
  size = "md",
  workers = [],
  keyboard = false,
}: {
  nodeDid: string | null | undefined;
  agentId: string | null | undefined;
  description?: string;
  size?: "sm" | "md";
  workers?: readonly SessionSummary[];
  keyboard?: boolean;
}) {
  const homeDid = useHomeDid();
  const found = useFleet((state) => nodeOf(state, nodeDid));
  const workersOn = useFleet(
    useShallow((state) => workers.map((w) => nodeOf(state, w.nodeDid))),
  );
  const node = found && !isWorkingNode(found, homeDid) ? found : null;
  const dim = size === "sm" ? "size-5 text-[9px]" : "size-7 text-[11px]";
  const overlap = size === "sm" ? "-ml-1.5" : "-ml-2";
  const workerAgents = [
    ...new Map(
      workers.flatMap((w, i) =>
        w.agentId && (w.agentId !== agentId || w.nodeDid !== nodeDid)
          ? [
              [
                JSON.stringify([w.nodeDid, w.agentId]),
                [w.agentId, workersOn[i] ?? null] as const,
              ] as const,
            ]
          : [],
      ),
    ),
  ];
  const workerNodes = [
    ...new Set(
      workersOn.filter(
        (n): n is NodeView => n !== null && !isWorkingNode(n, homeDid) && n !== node,
      ),
    ),
  ];
  const spread = node !== null || workerAgents.length > 0 || workerNodes.length > 0;
  const shownNodes = workerNodes.slice(0, 3);
  const shownAgents = workerAgents.slice(0, 3);
  const agentsLeft = workerAgents.length - shownAgents.length;
  const nodesLeft = workerNodes.length - shownNodes.length;
  const leftOut = agentsLeft + nodesLeft;
  const motion = "transition-[margin,box-shadow] duration-200 ease-out";
  const shifted = cn(
    "cursor-default ring-2 ring-background",
    motion,
    spread &&
      `${overlap} first:ml-0 group-hover/stack:ml-1 group-hover/stack:first:ml-0`,
    dim,
  );
  return (
    <span
      className={cn(
        "group/stack inline-flex shrink-0 items-center rounded-full py-0.5 transition-[padding,background-color] duration-200 ease-out",
        spread &&
          "hover:bg-raised hover:px-1 hover:shadow-xs hover:ring-1 hover:ring-border",
      )}
    >
      {node && (
        <NodeHoverCard deployment={node} side="bottom">
          <NodeAvatar name={node.node.displayName ?? node.label} className={shifted} />
        </NodeHoverCard>
      )}
      <AgentHoverCard
        deployment={found}
        agentId={agentId ?? null}
        description={description}
        side={spread ? "bottom" : "right"}
      >
        {keyboard ? (
          <button
            type="button"
            aria-label={`About ${agentName(agentId ?? null, found)} agent`}
            className="grid cursor-default place-items-center rounded-full p-0 leading-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:outline-none"
          >
            <AgentInitials
              name={agentName(agentId ?? null, found)}
              className={shifted}
            />
          </button>
        ) : (
          <AgentInitials name={agentName(agentId ?? null, found)} className={shifted} />
        )}
      </AgentHoverCard>
      {shownAgents.map(([identity, [b, on]]) => (
        <AgentHoverCard key={identity} deployment={on} agentId={b} side="bottom">
          <AgentInitials name={agentName(b, on)} className={shifted} />
        </AgentHoverCard>
      ))}
      {shownNodes.map((n) => (
        <NodeHoverCard key={nodeDidOf(n)} deployment={n} side="bottom">
          <NodeAvatar name={n.node.displayName ?? n.label} className={shifted} />
        </NodeHoverCard>
      ))}
      {leftOut > 0 && (
        <span
          className={cn(
            "grid shrink-0 place-items-center rounded-full border border-border bg-raised font-medium text-muted-foreground",
            shifted,
          )}
          title={[
            agentsLeft > 0 && `${agentsLeft} more agent${agentsLeft === 1 ? "" : "s"}`,
            nodesLeft > 0 && `${nodesLeft} more node${nodesLeft === 1 ? "" : "s"}`,
          ]
            .filter(Boolean)
            .join(" and ")}
        >
          +{leftOut}
        </span>
      )}
    </span>
  );
}
