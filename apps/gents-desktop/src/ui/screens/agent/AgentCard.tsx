import { useEffect, useState, type ReactNode } from "react";
import type {
  DesktopOperationsSnapshot,
  InstalledPack,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { nodeKeyOf, type NodeView } from "../../../hooks/fleetStore";
import { Badge } from "@gents/ui/components/badge";
import { cn } from "@gents/ui/lib/utils";
import { href, type Route } from "@/lib/router";
import { NodeAvatar } from "../AgentAvatar";
import { isLive } from "@/lib/live";
import { useApp } from "@/app/AppContext";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { useHomeDid, useNodeReach } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { InferenceLogos } from "../HoverCards";
import { healthy } from "./inferenceKinds";
import { RunNodeButton } from "./LocalServer";
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
    live ? `${live} ${live === 1 ? "session" : "sessions"} live` : null,
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
      className="grid min-w-0 gap-1 rounded-xl border border-border/60 px-3 py-2.5 transition-colors hover:bg-accent focus-visible:bg-accent focus-visible:outline-none"
    >
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="flex min-w-0 items-center gap-2 truncate text-sm">
        {children}
      </span>
    </a>
  );
}

function usePacks() {
  const { actions } = useApp();
  const [packs, setPacks] = useState<InstalledPack[] | null>(null);
  useEffect(() => {
    let alive = true;
    /* an adapter without the read, like one that fails it, leaves packs unknown */
    Promise.resolve()
      .then(() => actions.listInstalledPacks())
      .then(
        (report) => alive && setPacks(report.packs),
        () => alive && setPacks(null),
      );
    return () => {
      alive = false;
    };
  }, [actions]);
  return packs;
}

function packsText(packs: InstalledPack[] | null) {
  if (!packs) return "—";
  if (packs.length === 0) return "None installed";
  const broken = packs.filter((p) => p.error).length;
  const outdated = packs.filter((p) => p.outdated).length;
  return [
    `${packs.length} installed`,
    broken ? `${broken} with a problem` : null,
    outdated ? `${outdated} to update` : null,
  ]
    .filter(Boolean)
    .join(" · ");
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
  const sessions = useFleet((s) => s.sessionsOf[nodeKeyOf(deployment)] ?? NO_SESSIONS);
  useEffect(() => {
    let alive = true;
    void fetchOperationsSnapshot({ nodeDid: deployment.nodeDid }).then(
      (o) => alive && setOps(o),
      () => alive && setOps(null),
    );
    return () => {
      alive = false;
    };
  }, [fetchOperationsSnapshot, deployment.nodeDid, sessions]);
  const now = pulse(sessions, ops);
  const online = deployment.dialSucceeded;
  const backends = deployment.inferenceBackends;
  const unready = backends.filter((b) => !healthy(b.probeStatus)).length;
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
            <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <span
                aria-hidden
                className={cn(
                  "size-1.5 rounded-full",
                  online ? "bg-brand" : "bg-destructive",
                )}
              />
              {online ? "Online" : "Offline"}
            </span>
          </div>
          <p className="mt-1 truncate text-xs text-muted-foreground">
            {facts.join(" · ")}
            {/* the host's id in full only on hover; its end tells hosts apart */}
            {deployment.peerId && (
              <span title={deployment.peerId} className="font-mono">
                {facts.length ? " · " : ""}
                {deployment.peerId.slice(-8)}
              </span>
            )}
          </p>
        </div>
        {isLocalNode(deployment, homeDid) && <RunNodeButton />}
      </div>
      <div className="grid gap-2 @xl:grid-cols-3">
        <Summary label="Activity" to={{ name: "sessions" }}>
          <span
            aria-hidden
            className={cn(
              "size-1.5 shrink-0 rounded-full",
              now.overdue ? "bg-destructive" : now.busy ? "bg-brand" : "bg-border",
            )}
          />
          <span className="truncate">{now.text}</span>
        </Summary>
        <Summary label="Inference" to={section("inference")}>
          {backends.length > 0 && <InferenceLogos backends={backends} />}
          <span className="truncate">
            {backends.length === 0
              ? "None set up"
              : unready
                ? `${unready} not ready`
                : "Ready"}
          </span>
        </Summary>
        <Summary label="Packs" to={section("packs")}>
          <span className="truncate">{packsText(packs)}</span>
        </Summary>
      </div>
    </div>
  );
}
