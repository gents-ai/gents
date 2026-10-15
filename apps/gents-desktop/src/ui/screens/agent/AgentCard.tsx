/* Who this Node is, at the top of its settings: avatar, name, the facts
   that do not change (the DID stays in the Identity group below, where it
   can be copied), and one live line for what it is doing right now,
   read from the operations snapshot. The line links to the sessions list;
   the list's own filters do the rest. */
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
import { useToolAuthority } from "@/hooks/useClient";
import { NO_SESSIONS, useFleet } from "@/hooks/useFleet";

function pulse(
  sessions: readonly SessionSummary[],
  ops: DesktopOperationsSnapshot | null,
) {
  const live = sessions.filter((s) => isLive(s.turnState)).length;
  const tools = ops?.backgroundedTools ?? [];
  /* a started session is a background row of a session-message call */
  const worker = (t: (typeof tools)[number]) =>
    t.toolName === "agent_new" || t.toolName === "agent_message";
  const workers = tools.filter(worker).length;
  const jobs = tools.length - workers;
  const overdue = tools.filter((t) => t.deadlineExpired).length;
  const parts = [
    live ? `${live} ${live === 1 ? "session" : "sessions"} live` : null,
    workers ? `${workers} ${workers === 1 ? "session call" : "session calls"}` : null,
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
  const authority = useToolAuthority();
  const node = deployment.node;
  const name = node.displayName ?? deployment.label;
  const [ops, setOps] = useState<DesktopOperationsSnapshot | null>(null);
  /* the snapshot changes with every store ping; the session list is the cue */
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
    authority.ceiling ?? null,
    authority.root ?? null,
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
