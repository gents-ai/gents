import { useState, type ReactElement } from "react";
import { ArrowUpRight, Copy, MessageSquarePlus } from "lucide-react";
import { toast } from "sonner";
import type { NodeView } from "../../hooks/fleetStore";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@gents/ui/components/hover-card";
import { cn } from "@gents/ui/lib/utils";
import { href, navigate, type Route } from "@/lib/router";
import { useApp } from "@/app/AppContext";
import { agentReadiness } from "@/lib/agent-readiness";
import { access, agentName, bashAccess, fileAccess, network } from "./behavior";
import { agentOf, backendOf } from "@/lib/agents";
import { ProviderLogo } from "./ProviderLogo";
import { useProviderUsage } from "../hooks/useProviders";
import { closestWindow, resetsIn, usageLevel, UsageRing } from "./agent/ProviderUsage";
import { providerLogo } from "@/lib/provider";
import { useNodeReach } from "@/hooks/useClient";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { shortPath } from "@/lib/paths";
import { Skeleton } from "@gents/ui/components/skeleton";
import { SUBSCRIPTION } from "./agent/inferenceKinds";

/* the card looks the same at rest; a line that leads somewhere underlines
   only while pointed at or focused */
const LINK =
  "rounded-sm underline-offset-2 outline-none hover:underline focus-visible:underline";

function Line({
  label,
  title,
  to,
  children,
}: {
  label: string;
  title?: string;
  /** where the line leads, as a link */
  to?: Route;
  children: React.ReactNode;
}) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd title={title} className="min-w-0 truncate text-foreground">
        {to ? (
          <a href={href(to)} className={LINK}>
            {children}
          </a>
        ) : (
          children
        )}
      </dd>
    </>
  );
}

/* the agent's provider account at its closest limit; read only while the
   card is open, since the card's content mounts with it */
function AgentLimits({
  nodeDid,
  backend,
}: {
  nodeDid: string;
  backend: { backendId: string; providerKind?: string | null };
}) {
  /* only a subscription account reports usage; other backends have no
     limits to read, so their cards keep no place for one */
  if (!SUBSCRIPTION[backend.providerKind ?? ""]) return null;
  return <AccountLimits nodeDid={nodeDid} backendId={backend.backendId} />;
}

function AccountLimits({ nodeDid, backendId }: { nodeDid: string; backendId: string }) {
  const { usage, loading } = useProviderUsage(nodeDid);
  const view = usage.find((account) => account.backendIds.includes(backendId));
  const top = closestWindow(view);
  /* the line holds its place while the first read is out, so the card opens
     at its full height; a read with nothing for this account drops it */
  if (!view || !top)
    return loading ? (
      <Line label="Limits">
        <span aria-busy="true" className="flex h-4 items-center">
          <Skeleton className="h-3 w-28 rounded-sm bg-foreground/10" />
        </span>
      </Line>
    ) : null;
  const pct = Math.round(top.usedPct);
  const now = Date.now();
  return (
    <Line
      label="Limits"
      title={view.windows
        .map((w) =>
          [`${w.label} ${Math.round(w.usedPct)}%`, resetsIn(w, now)]
            .filter(Boolean)
            .join(", "),
        )
        .join(" · ")}
    >
      <span className="flex min-w-0 items-center gap-1.5 tabular-nums">
        <UsageRing pct={pct} className="size-3" />
        <span
          className={cn(
            usageLevel(pct) === "warning" && "text-warning",
            usageLevel(pct) === "danger" && "text-destructive",
          )}
        >
          {top.label} {pct}%
        </span>
        {resetsIn(top, now) && (
          <span className="truncate text-muted-foreground">· {resetsIn(top, now)}</span>
        )}
      </span>
    </Line>
  );
}

function CopyId({ did }: { did: string }) {
  return (
    <button
      type="button"
      aria-label="Copy node id"
      title="Copy node id"
      onClick={() =>
        void navigator.clipboard?.writeText(did).then(
          () => toast("Copied node id"),
          () => toast("Couldn’t copy the node id"),
        )
      }
      className="grid size-5 shrink-0 cursor-pointer place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:bg-accent focus-visible:outline-none"
    >
      <Copy className="size-3" />
    </button>
  );
}

const nodeSection = (nodeDid: string, section: string): Route => ({
  name: "agent",
  nodeDid,
  section,
});

const shortDid = (did: string) =>
  did.length > 22 ? `${did.slice(0, 12)}…${did.slice(-4)}` : did;

function NodeReach({ deployment }: { deployment: NodeView }) {
  const { local, root, ceiling, env } = useNodeReach(deployment);
  const home = useHomeFolder();
  return (
    <>
      <Line
        label="Can touch"
        title={root ?? undefined}
        to={nodeSection(deployment.nodeDid, "folders")}
      >
        {root ? shortPath(root, { home: local ? home : null, max: 40 }) : "—"}
      </Line>
      <Line label="At most" to={nodeSection(deployment.nodeDid, "agent")}>
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
          <a
            href={href(nodeSection(deployment.nodeDid, "agent"))}
            className={cn("font-heading text-base font-medium text-heading", LINK)}
          >
            {deployment.node.displayName ?? deployment.label}
          </a>
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
          <Line label="Agents" to={nodeSection(deployment.nodeDid, "agents")}>
            {deployment.agents.length}
          </Line>
          <Line label="Tasks" to={nodeSection(deployment.nodeDid, "tasks")}>
            {deployment.tasks.length}
          </Line>
          <Line label="Inference" to={nodeSection(deployment.nodeDid, "inference")}>
            <InferenceLogos backends={deployment.inferenceBackends} />
          </Line>
        </dl>
        <div className="mt-3 flex items-center gap-1">
          <p
            className="min-w-0 truncate font-mono text-[11px] text-muted-foreground"
            title={deployment.nodeDid}
          >
            {shortDid(deployment.nodeDid)}
          </p>
          <CopyId did={deployment.nodeDid} />
        </div>
      </HoverCardContent>
    </HoverCard>
  );
}

/* one logo per provider, however many of its accounts the node has; the
   accounts themselves are named on hover */
export function InferenceLogos({
  backends,
}: {
  backends: NodeView["inferenceBackends"];
}) {
  if (backends.length === 0) return <span className="text-muted-foreground">None</span>;
  const providers = new Map<string, (typeof backends)[number]>();
  for (const b of backends) {
    const key = providerLogo(b.providerKind, b.endpoint) ?? "other";
    if (!providers.has(key)) providers.set(key, b);
  }
  const shown = [...providers.values()];
  const names = backends.map((b) => b.name ?? b.backendId).join(", ");
  return (
    <span
      role="img"
      aria-label={names}
      title={names}
      className="-my-0.5 inline-flex items-center align-middle"
    >
      {shown.slice(0, 3).map((b, i) => (
        <span
          key={b.backendId}
          className={cn(
            "grid size-5 place-items-center rounded-full bg-muted ring-2 ring-popover",
            i > 0 && "-ml-1.5",
          )}
        >
          <ProviderLogo
            kind={b.providerKind}
            endpoint={b.endpoint}
            className="size-3"
          />
        </span>
      ))}
      {shown.length > 3 && (
        <span className="ml-1 text-muted-foreground tabular-nums">
          +{shown.length - 3}
        </span>
      )}
    </span>
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
        {summary && (
          <p
            title={summary}
            className="mt-1.5 line-clamp-2 text-xs leading-snug text-muted-foreground"
          >
            {summary}
          </p>
        )}
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
          <Line label="Status">
            {!b.enabled
              ? "Disabled"
              : readiness.ready
                ? "Enabled"
                : `Enabled, can’t run: ${readiness.reason}`}
          </Line>
          <Line
            label="Runs on"
            to={
              backend
                ? {
                    name: "agent",
                    nodeDid: deployment.nodeDid,
                    section: "inference",
                    item: backend.backendId,
                  }
                : undefined
            }
          >
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
          {backend && <AgentLimits nodeDid={deployment.nodeDid} backend={backend} />}
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
        <div className="mt-3 flex items-center gap-4 text-xs">
          <NewSessionWith
            nodeDid={deployment.nodeDid}
            agentId={b.agentId}
            enabled={b.enabled}
            onStart={() => setOpen(false)}
          />
          <a
            href={href({
              name: "agent",
              nodeDid: deployment.nodeDid,
              section: "agents",
              item: b.agentId,
            })}
            className={cn("inline-flex items-center gap-1 text-foreground", LINK)}
          >
            Settings <ArrowUpRight className="size-3" />
          </a>
        </div>
      </HoverCardContent>
    </HoverCard>
  );
}

/* in the card's content, so it reads the app only once the card is open */
function NewSessionWith({
  nodeDid,
  agentId,
  enabled,
  onStart,
}: {
  nodeDid: string;
  agentId: string;
  enabled: boolean;
  onStart: () => void;
}) {
  const { actions } = useApp();
  return (
    <button
      type="button"
      disabled={!enabled}
      onClick={() => {
        onStart();
        actions.selectNode(nodeDid);
        actions.selectAgent(agentId);
        navigate({ name: "session", sessionId: null });
      }}
      className={cn(
        "inline-flex cursor-pointer items-center gap-1 text-foreground disabled:cursor-default disabled:text-muted-foreground disabled:no-underline",
        LINK,
      )}
    >
      <MessageSquarePlus className="size-3" /> New session
    </button>
  );
}
