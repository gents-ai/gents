import { useEffect, useState, type ReactNode } from "react";
import type {
  DesktopOperationsSnapshot,
  InstalledPack,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { nodeKeyOf, type NodeView } from "../../../hooks/fleetStore";
import { Badge } from "@gents/ui/components/badge";
import { Skeleton } from "@gents/ui/components/skeleton";
import { href, type Route } from "@/lib/router";
import { NodeAvatar } from "../AgentAvatar";
import { isLive } from "@/lib/live";
import { useApp } from "@/app/AppContext";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { useHomeDid, useNodeReach } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { InferenceSummary, OnlineStatus, StateDot, counted } from "../NodeFacts";
import { ProviderLogo } from "../ProviderLogo";
import { agentOf, backendOf } from "@/lib/agents";
import { RunNodeButton } from "./LocalServer";
import { errorText } from "../../../lib/errorText";
import { shortPath } from "@/lib/paths";
import { NO_SESSIONS, useFleet } from "@/hooks/useFleet";

function pulse(
  sessions: readonly SessionSummary[],
  ops: DesktopOperationsSnapshot | null,
) {
  const live = sessions.filter((s) => isLive(s.turnState)).length;
  const tools = ops?.backgroundedTools ?? [];
  const workers = tools.filter((t) => t.startsSession).length;
  /* a plugin job is a workflow, not a background command */
  const workflows = tools.filter((t) => t.job && !t.startsSession).length;
  const jobs = tools.length - workers - workflows;
  const overdue = tools.filter((t) => t.deadlineExpired).length;
  const parts = [
    live ? `${counted(live, "session")} live` : null,
    workers ? `${workers} ${workers === 1 ? "session call" : "session calls"}` : null,
    workflows ? `${workflows} ${workflows === 1 ? "workflow" : "workflows"}` : null,
    jobs ? `${jobs} background ${jobs === 1 ? "job" : "jobs"}` : null,
    overdue ? `${overdue} past ${overdue === 1 ? "its" : "their"} deadline` : null,
  ].filter(Boolean) as string[];
  return {
    text: parts.length ? parts.join(" · ") : "Idle",
    busy: parts.length > 0,
    overdue,
  };
}

/* a card under the node's header: one thing about the node right now, and
   the way to where it is set */
function Summary({
  label,
  to,
  children,
}: {
  label: string;
  to: Route;
  children: ReactNode;
}) {
  return (
    <a
      href={href(to)}
      className="group/summary grid min-w-0 gap-1 rounded-xl bg-raised/50 px-3 py-2.5 transition-colors hover:bg-raised focus-visible:bg-raised focus-visible:outline-none"
    >
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="flex min-w-0 items-center gap-2 truncate text-sm underline-offset-2 group-hover/summary:underline group-focus-visible/summary:underline">
        {children}
      </span>
    </a>
  );
}

type PacksRead = InstalledPack[] | { failed: string } | undefined;

/** The node's installed packs: undefined while read, and why when the read
    failed. */
function usePacks(): PacksRead {
  const { actions } = useApp();
  const [packs, setPacks] = useState<PacksRead>(undefined);
  useEffect(() => {
    let alive = true;
    /* an adapter without the read, like one that fails it, leaves packs unknown */
    Promise.resolve()
      .then(() => actions.listInstalledPacks())
      .then(
        (report) => alive && setPacks(report.packs),
        (error: unknown) => alive && setPacks({ failed: errorText(error) }),
      );
    return () => {
      alive = false;
    };
  }, [actions]);
  return packs;
}

function PacksValue({ packs }: { packs: PacksRead }) {
  if (packs === undefined) return <Skeleton className="h-4 w-28" />;
  if (!Array.isArray(packs))
    return (
      <span title={packs.failed} className="truncate text-muted-foreground">
        Couldn’t read
      </span>
    );
  if (packs.length === 0)
    return (
      <span className="truncate">
        None installed ·{" "}
        <span className="underline-offset-2 group-hover/summary:underline group-focus-visible/summary:underline">
          Install a pack →
        </span>
      </span>
    );
  const broken = packs.filter((p) => p.error).length;
  const outdated = packs.filter((p) => p.outdated).length;
  return (
    <span className="truncate">
      {[
        `${packs.length} installed`,
        broken ? `${broken} with a problem` : null,
        outdated ? `${outdated} to update` : null,
      ]
        .filter(Boolean)
        .join(" · ")}
    </span>
  );
}

export function AgentCard({ deployment }: { deployment: NodeView }) {
  const {
    actions: { fetchOperationsSnapshot },
  } = useApp();
  const reach = useNodeReach(deployment);
  const home = useHomeFolder();
  const homeDid = useHomeDid();
  const packs = usePacks();
  const node = deployment.node;
  const name = node.displayName ?? deployment.label;
  const [ops, setOps] = useState<DesktopOperationsSnapshot | null>(null);
  /* background jobs come from a read; until its first answer the card waits */
  const [opsRead, setOpsRead] = useState(false);
  const sessions = useFleet((s) => s.sessionsOf[nodeKeyOf(deployment)] ?? NO_SESSIONS);
  useEffect(() => {
    let alive = true;
    void fetchOperationsSnapshot({ nodeDid: deployment.nodeDid }).then(
      (o) => {
        if (!alive) return;
        setOps(o);
        setOpsRead(true);
      },
      () => {
        if (!alive) return;
        setOps(null);
        setOpsRead(true);
      },
    );
    return () => {
      alive = false;
    };
  }, [fetchOperationsSnapshot, deployment.nodeDid, sessions]);
  const now = pulse(sessions, ops);
  const online = deployment.dialSucceeded;
  const backends = deployment.inferenceBackends;
  const section = (section: string): Route => ({
    name: "agent",
    nodeDid: deployment.nodeDid,
    section,
  });
  const facts = [
    reach.ceiling,
    reach.root && shortPath(reach.root, { home: reach.local ? home : null, max: 40 }),
  ].filter(Boolean) as string[];
  return (
    <div className="@container mb-8 grid gap-4">
      <div className="flex items-start gap-4">
        <NodeAvatar name={name} className="size-14" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="truncate font-heading text-lg font-medium text-heading">
              {name}
            </h2>
            {node.enabled === false && <Badge variant="outline">Disabled</Badge>}
            <OnlineStatus online={online} />
          </div>
          <p className="mt-1 truncate text-xs text-muted-foreground">
            {facts.join(" · ")}
            {/* the host's id in full only on hover; its end tells hosts apart */}
            {deployment.peerId && (
              <>
                {facts.length ? " · " : ""}
                <span title={deployment.peerId} className="font-mono">
                  {deployment.peerId.slice(-8)}
                </span>
              </>
            )}
          </p>
        </div>
        {isLocalNode(deployment, homeDid) && <RunNodeButton />}
      </div>
      <div className="grid gap-2 @xl:grid-cols-3">
        <Summary label="Activity" to={{ name: "sessions" }}>
          {opsRead ? (
            <>
              <StateDot tone={now.overdue ? "bad" : now.busy ? "busy" : "idle"} />
              <span className="truncate">{now.text}</span>
            </>
          ) : (
            <Skeleton className="h-4 w-24" />
          )}
        </Summary>
        <Summary label="Inference" to={section("inference")}>
          <InferenceSummary backends={backends} />
        </Summary>
        <Summary label="Packs" to={section("packs")}>
          <PacksValue packs={packs} />
        </Summary>
      </div>
    </div>
  );
}

/** The agent page's cards, as the node page's: what it runs on, what it is
    doing, and what runs with it on its own. */
export function AgentSummaryCards({
  deployment,
  agentId,
}: {
  deployment: NodeView;
  agentId: string;
}) {
  const agent = agentOf(deployment, agentId);
  const sessions = useFleet((s) => s.sessionsOf[nodeKeyOf(deployment)] ?? NO_SESSIONS);
  if (!agent) return null;
  const env = deployment.agentEnvironments.find((e) => e.agentId === agentId);
  const backend = backendOf(deployment, agent);
  const mine = sessions.filter((s) => s.agentId === agentId);
  const live = mine.filter((s) => isLive(s.turnState)).length;
  const tasks = deployment.tasks.filter((t) => t.agentId === agentId).length;
  const section = (section: string, item?: string): Route => ({
    name: "agent",
    nodeDid: deployment.nodeDid,
    section,
    ...(item ? { item } : {}),
  });
  return (
    <div className="@container mb-8">
      <div className="grid gap-2 @xl:grid-cols-3">
        <Summary
          label="Runs on"
          to={backend ? section("inference", backend.backendId) : section("inference")}
        >
          {backend && (
            <ProviderLogo kind={backend.providerKind} endpoint={backend.endpoint} />
          )}
          <span className="truncate">{env?.modelName ?? "No model"}</span>
        </Summary>
        <Summary label="Activity" to={{ name: "sessions" }}>
          <StateDot tone={live ? "busy" : "idle"} />
          <span className="truncate">
            {live
              ? `${counted(live, "session")} live`
              : `Idle · ${counted(mine.length, "session")}`}
          </span>
        </Summary>
        <Summary label="Tasks" to={section("tasks")}>
          <span className="truncate">{tasks ? counted(tasks, "task") : "None"}</span>
        </Summary>
      </div>
    </div>
  );
}
