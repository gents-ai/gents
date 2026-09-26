/* Who this agent is, at the top of its settings: avatar, name, the facts
   that do not change (the DID stays in the Identity group below, where it
   can be copied), and one live line for what it is doing right now,
   read from the operations snapshot. The line links to the sessions list;
   the list's own filters do the rest. */
import { useEffect, useState } from "react";
import { ArrowUpRight } from "lucide-react";
import type {
  DeploymentView,
  DesktopOperationsSnapshot,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import type { Shell } from "@/hooks/useShell";
import { href } from "@/lib/router";
import { AgentAvatar } from "../AgentAvatar";
import { isLive } from "@/lib/live";

function pulse(deployment: DeploymentView, ops: DesktopOperationsSnapshot | null) {
  const live = deployment.sessions.filter((s) => isLive(s.turnState)).length;
  const tools = ops?.backgroundedTools ?? [];
  /* a started session is a background row of a session-message call */
  const subagent = (t: (typeof tools)[number]) =>
    t.toolName === "create_session" || t.toolName === "send_message";
  const workers = tools.filter(subagent).length;
  const jobs = tools.length - workers;
  const overdue = tools.filter((t) => t.deadlineExpired).length;
  const parts = [
    live ? `${live} ${live === 1 ? "session" : "sessions"} live` : null,
    workers ? `${workers} ${workers === 1 ? "subagent" : "subagents"}` : null,
    jobs ? `${jobs} background ${jobs === 1 ? "job" : "jobs"}` : null,
    overdue ? `${overdue} past ${overdue === 1 ? "its" : "their"} deadline` : null,
  ].filter(Boolean) as string[];
  return {
    text: parts.length ? parts.join(" · ") : "Idle",
    busy: parts.length > 0,
    overdue,
  };
}

export function AgentCard({
  shell,
  deployment,
}: {
  shell: Shell;
  deployment: DeploymentView;
}) {
  const agent = deployment.agentPrincipal;
  const name = agent.displayName ?? deployment.label;
  const boot = shell.snapshot?.bootstrap;
  const [ops, setOps] = useState<DesktopOperationsSnapshot | null>(null);
  /* the snapshot changes with every store ping; the session list is the cue */
  const sessions = deployment.sessions;
  useEffect(() => {
    let alive = true;
    void shell.api.fetchOperationsSnapshot({ agentDid: deployment.agentDid }).then(
      (o) => alive && setOps(o),
      () => alive && setOps(null),
    );
    return () => {
      alive = false;
    };
  }, [shell.api, deployment.agentDid, sessions]);
  const now = pulse(deployment, ops);
  const facts = [
    boot?.initToolCeiling ?? null,
    boot?.initToolRoot ?? null,
    deployment.peerId ? `on ${deployment.peerId}` : null,
  ].filter(Boolean) as string[];
  return (
    <div className="mb-8 flex items-start gap-4">
      <AgentAvatar name={name} className="size-14" />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="truncate font-heading text-lg font-medium text-heading">
            {name}
          </h2>
          {agent.enabled === false && <Badge variant="outline">Disabled</Badge>}
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
