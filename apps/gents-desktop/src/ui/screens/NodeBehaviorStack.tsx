/* Who ran a session and where: the node's avatar behind, the behavior's in
   front. Each carries its own hover card. The node is looked up from the
   session's node id. The local node is where work is assumed to be, so
   only a remote node is shown; a session whose node the client cannot see
   shows the behavior alone too. */
import type { DeploymentView, SessionSummary } from "@source-inc/gents-desktop-client";
import { cn } from "@gents/ui/lib/utils";
import { isWorkingNode, nodeDidOf } from "@/lib/nodes";
import { AgentAvatar } from "./AgentAvatar";
import { AgentHoverCard, BehaviorHoverCard } from "./HoverCards";
import { BehaviorAvatar } from "./parts";
import { behaviorName } from "./behavior";

export function NodeBehaviorStack({
  nodes,
  homeDid,
  nodeDid,
  behaviorId,
  deployment,
  description,
  size = "md",
  workers = [],
  keyboard = false,
}: {
  nodes: readonly DeploymentView[];
  /** the home's agent DID, which marks the working node */
  homeDid: string | null | undefined;
  nodeDid: string | null | undefined;
  behaviorId: string | null | undefined;
  deployment: DeploymentView | null;
  description?: string;
  size?: "sm" | "md";
  /** sessions this one handed out: their behaviors, and any remote node
      they run on, join the stack so the row says the whole piece of work */
  workers?: readonly SessionSummary[];
  /** the behavior avatar is a labelled button, so its card opens from the keyboard */
  keyboard?: boolean;
}) {
  const found = nodes.find((n) => nodeDidOf(n) === nodeDid) ?? null;
  const node = found && !isWorkingNode(found, homeDid) ? found : null;
  const dim = size === "sm" ? "size-5 text-[9px]" : "size-7 text-[11px]";
  const overlap = size === "sm" ? "-ml-1.5" : "-ml-2";
  /* two marks overlap to read as one; hovering the pair spreads them on a
     pill so each can be read and hovered on its own */
  /* one mark per distinct behavior the workers use, beyond this one; one
     per distinct remote node beyond this one; three of each at most */
  const workerBehaviors = [
    ...new Set(workers.map((w) => w.behaviorId).filter((b) => b && b !== behaviorId)),
  ];
  const workerNodes = [
    ...new Set(
      workers
        .map((w) => nodes.find((n) => nodeDidOf(n) === w.agentDid) ?? null)
        .filter(
          (n): n is DeploymentView =>
            n !== null && !isWorkingNode(n, homeDid) && n !== node,
        ),
    ),
  ];
  const spread = node !== null || workerBehaviors.length > 0 || workerNodes.length > 0;
  /* the order says whose is whose: this session's node and behavior lead,
     then the workers' behaviors (folded into one count at rest), then any
     remote node a worker runs on, which always shows since where work runs
     matters at a glance. Hovering unfolds the behaviors, and the count stays
     only for what the cap left out */
  const shownNodes = workerNodes.slice(0, 3);
  const shownBehaviors = workerBehaviors.slice(0, 3);
  const leftOut =
    workerNodes.length -
    shownNodes.length +
    (workerBehaviors.length - shownBehaviors.length);
  /* folded marks keep a zero-width box rather than leaving the layout: a
     hover card anchored to one is still closing when the pointer leaves,
     and an anchor with no box sends it to the page's corner */
  /* every mark moves with the same curve: width, margin, fade and ring
     together, so the fold reads as one motion rather than parts popping */
  const motion = "transition-[width,margin,opacity,box-shadow] duration-200 ease-out";
  const foldedBehavior = cn(
    "ml-0 w-0 overflow-hidden opacity-0 ring-0",
    size === "sm" ? "group-hover/stack:w-5" : "group-hover/stack:w-7",
    "group-hover/stack:opacity-100 group-hover/stack:ring-2",
  );
  /* the count folds the other way: there at rest, gone once its marks are out */
  const foldedCount = cn(
    "overflow-hidden",
    leftOut === 0 &&
      "group-hover/stack:ml-0 group-hover/stack:w-0 group-hover/stack:opacity-0 group-hover/stack:ring-0",
  );
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
        /* the vertical padding is there at rest too, so the pill never
           changes the row's height when it appears */
        "group/stack inline-flex shrink-0 items-center rounded-full py-0.5 transition-[padding,background-color] duration-200 ease-out",
        spread &&
          "hover:bg-raised hover:px-1 hover:shadow-xs hover:ring-1 hover:ring-border",
      )}
    >
      {node && (
        <AgentHoverCard deployment={node} side="bottom">
          <AgentAvatar
            name={node.agentPrincipal.displayName ?? node.label}
            className={shifted}
          />
        </AgentHoverCard>
      )}
      <BehaviorHoverCard
        deployment={deployment}
        behaviorId={behaviorId ?? null}
        description={description}
        side={spread ? "bottom" : "right"}
      >
        {keyboard ? (
          /* a button, so the card opens from the keyboard too: the one
             place a screen offers this, so the name is found once */
          <button
            type="button"
            aria-label={`About ${behaviorName(behaviorId ?? null, deployment)} behavior`}
            className="grid cursor-default place-items-center rounded-full p-0 leading-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:outline-none"
          >
            <BehaviorAvatar
              name={behaviorName(behaviorId ?? null, deployment)}
              behaviorId={behaviorId}
              className={shifted}
            />
          </button>
        ) : (
          <BehaviorAvatar
            name={behaviorName(behaviorId ?? null, deployment)}
            behaviorId={behaviorId}
            className={shifted}
          />
        )}
      </BehaviorHoverCard>
      {shownBehaviors.map((b) => (
        <BehaviorHoverCard key={b} deployment={deployment} behaviorId={b} side="bottom">
          <BehaviorAvatar
            name={behaviorName(b, deployment)}
            behaviorId={b}
            className={cn(shifted, foldedBehavior)}
          />
        </BehaviorHoverCard>
      ))}
      {shownNodes.map((n) => (
        <AgentHoverCard key={nodeDidOf(n)} deployment={n} side="bottom">
          <AgentAvatar
            name={n.agentPrincipal.displayName ?? n.label}
            className={shifted}
          />
        </AgentHoverCard>
      ))}
      {workers.length > 0 && (
        <span
          className={cn(
            "grid shrink-0 place-items-center rounded-full border border-border bg-raised font-medium text-muted-foreground",
            shifted,
            foldedCount,
          )}
          title={`${workers.length} worker${workers.length === 1 ? "" : "s"}`}
        >
          {leftOut > 0 ? (
            <>
              <span className="group-hover/stack:hidden">+{workers.length}</span>
              <span className="hidden group-hover/stack:inline">+{leftOut}</span>
            </>
          ) : (
            <span>+{workers.length}</span>
          )}
        </span>
      )}
    </span>
  );
}
