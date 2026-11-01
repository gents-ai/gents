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
import { providerLogo } from "@/lib/provider";
import { useNodeReach } from "@/hooks/useClient";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { shortPath } from "@/lib/paths";

/* the card looks the same at rest; what leads somewhere underlines only
   while pointed at or focused */
const LINK =
  "rounded-sm underline-offset-2 outline-none hover:underline focus-visible:underline";

const ROW = "col-span-2 grid grid-cols-subgrid";

/** One fact of a card. One that leads somewhere is a single link over its
    label and value: one stop for the keyboard, and a wide target. */
function Line({
  label,
  title,
  to,
  children,
}: {
  label: string;
  title?: string;
  /** where the line leads */
  to?: Route;
  children: React.ReactNode;
}) {
  const cells = (
    <>
      <span className="text-muted-foreground transition-colors group-hover/line:text-foreground/80 group-focus-visible/line:text-foreground/80">
        {label}
      </span>
      <span
        title={title}
        className="min-w-0 truncate text-foreground underline-offset-2 group-hover/line:underline group-focus-visible/line:underline"
      >
        {children}
      </span>
    </>
  );
  return to ? (
    <a href={href(to)} className={cn(ROW, "group/line rounded-sm outline-none")}>
      {cells}
    </a>
  ) : (
    <div className={ROW}>{cells}</div>
  );
}

function Facts({ children }: { children: React.ReactNode }) {
  return (
    <div className="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
      {children}
    </div>
  );
}

/** A card's first line: its name, leading to its page, and its state. */
function CardTitle({
  name,
  to,
  children,
}: {
  name: string;
  to: Route;
  children?: React.ReactNode;
}) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <a
        href={href(to)}
        className={cn(
          "min-w-0 truncate font-heading text-base font-medium text-heading",
          LINK,
        )}
      >
        {name}
      </a>
      {children}
    </div>
  );
}

/** A card's last line, the same on every card: start a session, or open
    its settings. */
function CardActions({
  nodeDid,
  agentId,
  enabled,
  settings,
  onStart,
}: {
  nodeDid: string;
  /** the node's default agent when none */
  agentId: string | null;
  enabled: boolean;
  settings: Route;
  onStart: () => void;
}) {
  return (
    <div className="mt-3 flex items-center gap-4 text-xs">
      <NewSessionWith
        nodeDid={nodeDid}
        agentId={agentId}
        enabled={enabled}
        onStart={onStart}
      />
      <a
        href={href(settings)}
        className={cn("inline-flex items-center gap-1 text-foreground", LINK)}
      >
        Settings <ArrowUpRight className="size-3" />
      </a>
    </div>
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
  const [open, setOpen] = useState(false);
  const online = deployment.dialSucceeded;
  const page = nodeSection(deployment.nodeDid, "agent");
  return (
    <HoverCard open={open} onOpenChange={setOpen}>
      <HoverCardTrigger delay={500} render={children} />
      <HoverCardContent side={side} align="start" className="w-80">
        <CardTitle name={deployment.node.displayName ?? deployment.label} to={page}>
          <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
            <span
              className={cn(
                "size-2 rounded-full",
                online ? "bg-brand" : "bg-destructive",
              )}
            />
            {online ? "Online" : "Offline"}
          </span>
        </CardTitle>
        <Facts>
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
          <Line label="ID" title={deployment.nodeDid}>
            <span className="inline-flex max-w-full items-center gap-1 align-middle">
              <span className="truncate font-mono text-[11px] text-muted-foreground">
                {shortDid(deployment.nodeDid)}
              </span>
              <CopyId did={deployment.nodeDid} />
            </span>
          </Line>
        </Facts>
        <CardActions
          nodeDid={deployment.nodeDid}
          agentId={null}
          enabled={deployment.node.enabled !== false}
          settings={page}
          onStart={() => setOpen(false)}
        />
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
  const tasks = deployment.tasks.filter((t) => t.agentId === b.agentId).length;
  const settings: Route = {
    name: "agent",
    nodeDid: deployment.nodeDid,
    section: "agents",
    item: b.agentId,
  };
  return (
    <HoverCard open={open} onOpenChange={setOpen}>
      <HoverCardTrigger delay={500} render={children} onFocus={() => setOpen(true)} />
      <HoverCardContent
        side={side}
        align="start"
        className="w-80"
        data-testid="agent-hover-card"
      >
        <CardTitle name={agentName(agentId, deployment)} to={settings}>
          {b.isDefault && (
            <span className="shrink-0 text-xs text-muted-foreground">Default</span>
          )}
        </CardTitle>
        {summary && (
          <p
            title={summary}
            className="mt-1.5 line-clamp-2 text-xs leading-snug text-muted-foreground"
          >
            {summary}
          </p>
        )}
        <Facts>
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
          {tasks > 0 && (
            <Line label="Tasks" to={nodeSection(deployment.nodeDid, "tasks")}>
              {tasks}
            </Line>
          )}
        </Facts>
        <CardActions
          nodeDid={deployment.nodeDid}
          agentId={b.agentId}
          enabled={b.enabled}
          settings={settings}
          onStart={() => setOpen(false)}
        />
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
  agentId: string | null;
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
