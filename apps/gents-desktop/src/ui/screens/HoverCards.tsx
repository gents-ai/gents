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
import { agentOf, backendOf } from "@/lib/agents";
import { ProviderLogo } from "./ProviderLogo";
import { useNodeReach } from "@/hooks/useClient";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { shortPath } from "@/lib/paths";

function Line({
  label,
  title,
  children,
}: {
  label: string;
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd title={title} className="min-w-0 truncate text-foreground">
        {children}
      </dd>
    </>
  );
}

const shortDid = (did: string) =>
  did.length > 22 ? `${did.slice(0, 12)}…${did.slice(-4)}` : did;

function NodeReach({ deployment }: { deployment: NodeView }) {
  const { local, root, ceiling, env } = useNodeReach(deployment);
  const home = useHomeFolder();
  return (
    <>
      <Line label="Can touch" title={root ?? undefined}>
        {root ? shortPath(root, { home: local ? home : null, max: 40 }) : "—"}
      </Line>
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
  /** where the card opens; a neighbor that must stay hoverable wants 'bottom' */
  side?: "right" | "bottom" | "top" | "left";
  children: ReactElement;
}) {
  const online = deployment.dialSucceeded;
  return (
    <HoverCard>
      <HoverCardTrigger delay={500} render={children} />
      <HoverCardContent side={side} align="start" className="w-80">
        <div className="flex items-baseline justify-between gap-3">
          <span className="font-heading text-base font-medium text-heading">
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
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
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
  const backend = backendOf(deployment, b);
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
          <span className="font-heading text-base font-medium text-heading">
            {agentName(agentId, deployment)}
          </span>
          {b.isDefault && (
            <span className="text-xs text-muted-foreground">Default</span>
          )}
        </div>
        {summary && <p className="mt-1.5 text-sm text-muted-foreground">{summary}</p>}
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
          <Line label="Status">
            {!b.enabled
              ? "Disabled"
              : readiness.ready
                ? "Enabled"
                : `Enabled, can’t run: ${readiness.reason}`}
          </Line>
          <Line label="Runs on">
            {env?.modelName ? (
              <span className="flex min-w-0 items-center gap-1.5">
                {backend && (
                  <ProviderLogo
                    kind={backend.providerKind}
                    endpoint={backend.endpoint}
                    className="size-3"
                  />
                )}
                <span className="truncate">{env.modelName}</span>
              </span>
            ) : (
              "no backend"
            )}
          </Line>
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
          className="mt-3 inline-flex items-center gap-1 text-xs text-foreground underline-offset-2 hover:underline"
        >
          Settings <ArrowUpRight className="size-3" />
        </a>
      </HoverCardContent>
    </HoverCard>
  );
}
