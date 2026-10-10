/* Agents: the node's list, each row's controls, and the default it
   runs; a agent opens in its editor. */
import type { NodeView } from "../../../hooks/fleetStore";
import { setEnabled } from "./enabled";
import { useState } from "react";
import { ArrowLeft, MoreHorizontal } from "lucide-react";
import type { AgentView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Switch } from "@gents/ui/components/switch";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { toast } from "sonner";
import { href, navigate } from "@/lib/router";
import { agentReadiness } from "@/lib/agent-readiness";
import { shortAccess } from "../behavior";
import { AgentInitials } from "../parts";
import { ListDetail } from "./ListDetail";
import { toastFailure } from "@/lib/failure";
import { agentOf } from "@/lib/agents";
import { useApp } from "@/app/AppContext";
import type { ShellActions } from "@/../hooks/shellActions";
import { listNames, newAgentView } from "./behaviorDraft";
import { AgentEditor } from "./BehaviorEditor";

/* one-click changes that a agent's row and its header share */

/* One apply enables the agent and names it the default: publication
   rejects a disabled default, and it decides whether the agent can run. */
export async function saveDefault(
  changeConfig: ShellActions["changeConfig"],
  deployment: NodeView,
  agentId: string,
) {
  await changeConfig("setDefaultAgent", {
    nodeDid: deployment.nodeDid,
    agentId,
  });
}

export const DEFAULT_STAYS_ENABLED =
  "The default agent stays enabled. Choose another default before turning it off.";

/* the end of a agent's row: its enable switch and a menu */
function RowControls({
  deployment,
  agent,
  inEditor = false,
}: {
  deployment: NodeView;
  agent: AgentView;
  /* at the top of the agent's page: the switch says its state, and there is no Edit */
  inEditor?: boolean;
}) {
  const { changeConfig } = useApp().actions;
  const [busy, setBusy] = useState(false);
  /* the current default is never turned off in place */
  const keptOn = agent.enabled && agent.isDefault;
  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      await setEnabled(changeConfig, deployment.nodeDid, "Agent", agent.agentId, next);
      toast(`${agent.displayName} is ${next ? "enabled" : "disabled"}`);
    } catch (e) {
      toastFailure(`turn it ${next ? "on" : "off"}`, e);
    } finally {
      setBusy(false);
    }
  };
  const makeDefault = async () => {
    try {
      await saveDefault(changeConfig, deployment, agent.agentId);
      toast(
        agent.enabled
          ? "Default agent set"
          : `${agent.displayName} is enabled and is now the default`,
      );
    } catch (e) {
      toastFailure("set the default agent", e);
    }
  };
  return (
    <div
      className="flex items-center gap-1"
      data-testid={inEditor ? "agent-status" : "agent-row-controls"}
    >
      <span
        className="flex items-center gap-2.5 px-1 text-sm"
        title={keptOn ? DEFAULT_STAYS_ENABLED : undefined}
      >
        {inEditor && (agent.enabled ? "Enabled" : "Disabled")}
        <Switch
          aria-label={`${agent.displayName} is ${agent.enabled ? "enabled" : "disabled"}`}
          aria-description={keptOn ? DEFAULT_STAYS_ENABLED : undefined}
          checked={agent.enabled}
          disabled={busy || keptOn}
          onCheckedChange={(v) => void toggle(v)}
        />
      </span>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="quiet"
              size="icon-sm"
              aria-label={`More for ${agent.displayName}`}
            />
          }
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-auto min-w-44">
          <DropdownMenuGroup>
            <DropdownMenuItem
              className="whitespace-nowrap"
              disabled={agent.isDefault}
              onClick={() => void makeDefault()}
            >
              {agent.isDefault ? (
                "The default agent"
              ) : (
                <span className="flex flex-col">
                  <span>Make default</span>
                  {!agent.enabled && (
                    <span className="text-xs text-muted-foreground">
                      Also enables it
                    </span>
                  )}
                </span>
              )}
            </DropdownMenuItem>
            {!inEditor && (
              <DropdownMenuItem
                className="whitespace-nowrap"
                render={
                  <a
                    href={href({
                      name: "agent",
                      nodeDid: deployment.nodeDid,
                      section: "agents",
                      item: agent.agentId,
                    })}
                  />
                }
              >
                Edit
              </DropdownMenuItem>
            )}
          </DropdownMenuGroup>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

export function AgentsPanel({
  deployment,
  agentId,
}: {
  deployment: NodeView;
  agentId?: string;
}) {
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
    section: "agents",
  };
  /* New agent: a draft page, nothing saved until Save */
  const [draft, setDraft] = useState<AgentView | null>(null);
  const inUse = new Set(deployment.agents.map((b) => b.contextId));
  const unusedContexts = deployment.contexts.filter(
    (c) => !inUse.has(c.context_id),
  ).length;
  /* who uses each context, so a row can say when its instructions are shared */
  const byContext = new Map<string, AgentView[]>();
  for (const b of deployment.agents) {
    if (!b.contextId) continue;
    const list = byContext.get(b.contextId);
    if (list) list.push(b);
    else byContext.set(b.contextId, [b]);
  }
  const contextIds = new Set(deployment.contexts.map((c) => c.context_id));
  const line = (b: AgentView) => {
    const readiness = agentReadiness(deployment, b.agentId);
    if (!readiness.ready) return `unavailable: ${readiness.reason}`;
    const e = deployment.agentEnvironments.find((x) => x.agentId === b.agentId);
    const sharing = (b.contextId ? (byContext.get(b.contextId) ?? []) : []).filter(
      (x) => x.agentId !== b.agentId,
    );
    return [
      e?.modelName ?? "no backend",
      `files ${shortAccess(e?.fileAccess)}`,
      `bash ${shortAccess(e?.bashAccess)}`,
      ...(!b.contextId || !contextIds.has(b.contextId) ? ["no instructions"] : []),
      ...(sharing.length
        ? [`shared with ${listNames(sharing.map((x) => x.displayName))}`]
        : []),
    ].join(" · ");
  };
  if (draft)
    return (
      <div>
        <div className="mb-6 flex items-center justify-between gap-3">
          <button
            type="button"
            onClick={() => setDraft(null)}
            className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> Agents
          </button>
        </div>
        <AgentEditor
          key={draft.agentId}
          deployment={deployment}
          agent={draft}
          draft={{
            onSaved: (id) => {
              setDraft(null);
              navigate({ ...base, item: id });
            },
            onCancel: () => setDraft(null),
          }}
        />
      </div>
    );
  return (
    <>
      <ListDetail
        base={base}
        item={agentId}
        toolbar={(id) => {
          const b = agentOf(deployment, id);
          return b ? <RowControls deployment={deployment} agent={b} inEditor /> : null;
        }}
        /* the default is pinned first and named beside its title */
        rows={[...deployment.agents]
          .sort((a, b) => Number(b.isDefault) - Number(a.isDefault))
          .map((b) => ({
            tags: b.tags,
            id: b.agentId,
            title: b.displayName,
            titleNote: b.isDefault ? "Default" : undefined,
            meta: line(b),
            metaMono: true,
            trailing: <RowControls deployment={deployment} agent={b} />,
            icon: (
              <AgentInitials
                name={b.displayName}
                className="size-6 border-0 bg-transparent text-[10px]"
              />
            ),
          }))}
        createLabel="New agent"
        empty="No agents yet. A agent is what an agent is told, what it may use, and what runs it."
        onCreate={() => setDraft(newAgentView(deployment))}
        detail={(id) => {
          const agent = agentOf(deployment, id)!;
          return (
            <AgentEditor key={agent.agentId} deployment={deployment} agent={agent} />
          );
        }}
      />
      {!agentId && unusedContexts > 0 && (
        <p data-testid="unused-contexts" className="mt-3 text-sm text-muted-foreground">
          {unusedContexts === 1
            ? "1 context isn’t used by any agent"
            : `${unusedContexts} contexts aren’t used by any agent`}{" "}
          ·{" "}
          <a
            href={href({ ...base, section: "contexts" })}
            className="text-foreground underline-offset-2 hover:underline"
          >
            Review
          </a>
        </p>
      )}
    </>
  );
}
