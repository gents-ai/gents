import { useEffect, useState } from "react";
import { ArrowUpRight } from "lucide-react";
import type {
  DesktopOperationsSnapshot,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { nodeKeyOf, type NodeView } from "../../../hooks/fleetStore";
import { Badge } from "@gents/ui/components/badge";
import { href } from "@/lib/router";
import { NodeAvatar } from "../AgentAvatar";
import { isLive } from "@/lib/live";
import { useApp } from "@/app/AppContext";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { useNodeReach } from "@/hooks/useClient";
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

export function AgentCard({ deployment }: { deployment: NodeView }) {
  const {
    actions: { fetchOperationsSnapshot },
  } = useApp();
  const reach = useNodeReach(deployment);
  const home = useHomeFolder();
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
  const facts = [
    reach.ceiling,
    reach.root && shortPath(reach.root, { home: reach.local ? home : null, max: 40 }),
    deployment.peerId ? `on ${deployment.peerId}` : null,
  ].filter(Boolean) as string[];
  return (
    <div className="mb-8 flex items-start gap-4">
      <NodeAvatar name={name} className="size-14" />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="truncate font-heading text-lg font-medium text-heading">
            {name}
          </h2>
          {node.enabled === false && <Badge variant="outline">Disabled</Badge>}
        </div>
        {facts.length > 0 && (
          <p className="mt-1 truncate text-xs text-muted-foreground">
            {facts.join(" · ")}
          </p>
        )}
        <a
          href={href({ name: "sessions" })}
          className="mt-2 inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
        >
          <span
            className={
              now.overdue
                ? "size-1.5 rounded-full bg-destructive"
                : now.busy
                  ? "size-1.5 rounded-full bg-brand"
                  : "size-1.5 rounded-full bg-border"
            }
            aria-hidden="true"
          />
          {now.text}
          <ArrowUpRight className="size-3" />
        </a>
      </div>
    </div>
  );
}
