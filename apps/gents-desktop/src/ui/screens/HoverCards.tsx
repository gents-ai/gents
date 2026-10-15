/* Delayed cards on avatars: what a reader most wants to know without
   opening anything. The node: online, what it may touch, how much it
   has, its DID. An agent: what it is for, whether it is enabled, what it
   runs on, its access, whether its instructions are shared, and a way to
   its settings. Shown where an avatar stands in for an agent, not where
   the agent is already on screen. */
import { useState, type ReactElement } from "react";
import { ArrowUpRight } from "lucide-react";
import type { NodeView } from "../../hooks/fleetStore";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@gents/ui/components/hover-card";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { agentReadiness } from "@/lib/agent-readiness";
import { access, agentName, bashAccess, fileAccess, network } from "./behavior";
import { agentOf } from "@/lib/agents";
import { useHomeDid, useToolAuthority } from "@/hooks/useClient";
import { isWorkingNode } from "@/lib/nodes";

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 truncate text-foreground">{children}</dd>
    </>
  );
}

const shortDid = (did: string) =>
  did.length > 22 ? `${did.slice(0, 12)}…${did.slice(-4)}` : did;

/* What a node may touch. The tool root and ceiling this machine was set up
   with are its own node's; any other node says it through its default
   agent's environment. */
function NodeReach({ deployment }: { deployment: NodeView }) {
  const homeDid = useHomeDid();
  const own = useToolAuthority();
  const { root, ceiling } = isWorkingNode(deployment, homeDid)
    ? own
    : { root: null, ceiling: null };
  const env =
    deployment.agentEnvironments.find((e) => e.isDefault) ??
    deployment.agentEnvironments[0];
  return (
    <>
      <Line label="Can touch">{root ?? env?.workspaceRoot ?? "—"}</Line>
      <Line label="At most">
        {ceiling ? access(ceiling) : env ? access(env.fileAccess) : "—"}
        {env ? `, ${network(env.networkAccess)}` : ""}
      </Line>
    </>
  );
}

export function NodeHoverCard({
  deployment,
  side = "right",
  children,
}: {
  deployment: NodeView;
  /** where the card opens; the default suits the rail and lists, a
      neighbor that must stay hoverable wants 'bottom' */
  side?: "right" | "bottom" | "top" | "left";
  children: ReactElement;
}) {
  const online = deployment.dialSucceeded;
  return (
    <HoverCard>
      <HoverCardTrigger delay={500} render={children} />
      <HoverCardContent side={side} align="start" className="w-80">
        <div className="flex items-baseline justify-between gap-3">
          <span className="font-heading text-lg font-medium text-heading">
            {deployment.node.displayName ?? deployment.label}
          </span>
          <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <span
              className={cn(
                "size-2 rounded-full",
                online ? "bg-brand" : "bg-destructive",
              )}
            />
            {online ? "Online" : "Offline"}
          </span>
        </div>
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
          <NodeReach deployment={deployment} />
          <Line label="Agents">{deployment.agents.length}</Line>
          <Line label="Tasks">{deployment.tasks.length}</Line>
          <Line label="Inference">{deployment.inferenceBackends.length}</Line>
        </dl>
        <p
          className="mt-3 truncate font-mono text-[11px] text-muted-foreground"
          title={deployment.nodeDid}
        >
          {shortDid(deployment.nodeDid)}
        </p>
      </HoverCardContent>
    </HoverCard>
  );
}

export function AgentHoverCard({
  deployment,
  agentId,
  description,
  side = "right",
  children,
}: {
  deployment: NodeView | null;
  agentId: string | null;
  description?: string;
  side?: "right" | "bottom" | "top" | "left";
  children: ReactElement;
}) {
  const [open, setOpen] = useState(false);
  const b = agentOf(deployment, agentId);
  const env = deployment?.agentEnvironments.find((e) => e.agentId === agentId);
  if (!deployment || !b) return children;
  const readiness = agentReadiness(deployment, b.agentId);
  const hasInstructions = Boolean(
    b.contextId &&
    deployment.contexts.some((context) => context.context_id === b.contextId),
  );
  const sharing = hasInstructions
    ? deployment.agents.filter(
        (agent) => agent.contextId === b.contextId && agent.agentId !== b.agentId,
      )
    : [];
  const summary = description || b.description || undefined;
  return (
    <HoverCard open={open} onOpenChange={setOpen}>
      <HoverCardTrigger delay={500} render={children} onFocus={() => setOpen(true)} />
      <HoverCardContent
        side={side}
        align="start"
        className="w-80"
        data-testid="agent-hover-card"
      >
        <div className="flex items-baseline justify-between gap-3">
          <span className="font-heading text-lg font-medium text-heading">
            {agentName(agentId, deployment)}
          </span>
          {b.isDefault && (
            <span className="text-xs text-muted-foreground">Default</span>
          )}
        </div>
        {summary && <p className="mt-1.5 text-sm text-muted-foreground">{summary}</p>}
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
          <Line label="Status">
            {!b.enabled
              ? "Disabled"
              : readiness.ready
                ? "Enabled"
                : `Enabled, can’t run: ${readiness.reason}`}
          </Line>
          <Line label="Runs on">{env?.modelName ?? "no backend"}</Line>
          <Line label="Files">{fileAccess(env?.fileAccess ?? "off")}</Line>
          <Line label="Commands">{bashAccess(env?.bashAccess ?? "off")}</Line>
          <Line label="Network">{network(env?.networkAccess)}</Line>
          {(!hasInstructions || sharing.length > 0) && (
            <Line label="Instructions">
              {!hasInstructions
                ? "None yet"
                : `Shared with ${
                    sharing.length <= 2
                      ? sharing.map((agent) => agent.displayName).join(" and ")
                      : `${sharing[0]!.displayName} and ${sharing.length - 1} more`
                  }`}
            </Line>
          )}
          <Line label="Sessions">{env?.sessionCount ?? 0}</Line>
        </dl>
        <a
          href={href({
            name: "agent",
            nodeDid: deployment.nodeDid,
            section: "agents",
            item: b.agentId,
          })}
          className="mt-3 inline-flex items-center gap-1 text-sm text-foreground underline-offset-2 hover:underline"
        >
          Settings <ArrowUpRight className="size-3.5" />
        </a>
      </HoverCardContent>
    </HoverCard>
  );
}
