/* Behaviors: the node's list, each row's controls, and the default it
   runs; a behavior opens in its editor. */
import type { NodeView } from "../../../hooks/fleetStore";
import { setEnabled } from "./enabled";
import { useState } from "react";
import { ArrowLeft, MoreHorizontal } from "lucide-react";
import type { BehaviorView } from "@source-inc/gents-desktop-client";
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
import { behaviorReadiness } from "@/lib/behavior-readiness";
import { shortAccess } from "../behavior";
import { BehaviorAvatar } from "../parts";
import { ListDetail } from "./ListDetail";
import { toastFailure } from "@/lib/failure";
import { agentOf } from "@/lib/agents";
import { useApp } from "@/app/AppContext";
import type { ShellActions } from "@/../hooks/shellActions";
import { listNames, newBehaviorView } from "./behaviorDraft";
import { BehaviorEditor } from "./BehaviorEditor";

/* one-click changes that a behavior's row and its header share */

/* One apply enables the behavior and names it the default: publication
   rejects a disabled default, and it decides whether the behavior can run. */
export async function saveDefault(
  changeConfig: ShellActions["changeConfig"],
  deployment: NodeView,
  behaviorId: string,
) {
  await changeConfig("setDefaultBehavior", {
    agentDid: deployment.agentDid,
    behaviorId,
  });
}

export const DEFAULT_STAYS_ENABLED =
  "The default behavior stays enabled. Choose another default before turning it off.";

/* the end of a behavior's row: its enable switch and a menu */
function RowControls({
  deployment,
  behavior,
  inEditor = false,
}: {
  deployment: NodeView;
  behavior: BehaviorView;
  /* at the top of the behavior's page: the switch says its state, and there is no Edit */
  inEditor?: boolean;
}) {
  const { changeConfig } = useApp().actions;
  const [busy, setBusy] = useState(false);
  /* the current default is never turned off in place */
  const keptOn = behavior.enabled && behavior.isDefault;
  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      await setEnabled(
        changeConfig,
        deployment.agentDid,
        "AgentBehavior",
        behavior.behaviorId,
        next,
      );
      toast(`${behavior.displayName} is ${next ? "enabled" : "disabled"}`);
    } catch (e) {
      toastFailure(`turn it ${next ? "on" : "off"}`, e);
    } finally {
      setBusy(false);
    }
  };
  const makeDefault = async () => {
    try {
      await saveDefault(changeConfig, deployment, behavior.behaviorId);
      toast(
        behavior.enabled
          ? "Default behavior set"
          : `${behavior.displayName} is enabled and is now the default`,
      );
    } catch (e) {
      toastFailure("set the default behavior", e);
    }
  };
  return (
    <div
      className="flex items-center gap-1"
      data-testid={inEditor ? "behavior-status" : "behavior-row-controls"}
    >
      <span
        className="flex items-center gap-2.5 px-1 text-sm"
        title={keptOn ? DEFAULT_STAYS_ENABLED : undefined}
      >
        {inEditor && (behavior.enabled ? "Enabled" : "Disabled")}
        <Switch
          aria-label={`${behavior.displayName} is ${behavior.enabled ? "enabled" : "disabled"}`}
          aria-description={keptOn ? DEFAULT_STAYS_ENABLED : undefined}
          checked={behavior.enabled}
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
              aria-label={`More for ${behavior.displayName}`}
            />
          }
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-auto min-w-44">
          <DropdownMenuGroup>
            <DropdownMenuItem
              className="whitespace-nowrap"
              disabled={behavior.isDefault}
              onClick={() => void makeDefault()}
            >
              {behavior.isDefault ? (
                "The default behavior"
              ) : (
                <span className="flex flex-col">
                  <span>Make default</span>
                  {!behavior.enabled && (
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
                      agentDid: deployment.agentDid,
                      section: "behaviors",
                      item: behavior.behaviorId,
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

export function BehaviorsPanel({
  deployment,
  behaviorId,
}: {
  deployment: NodeView;
  behaviorId?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "behaviors",
  };
  /* New behavior: a draft page, nothing saved until Save */
  const [draft, setDraft] = useState<BehaviorView | null>(null);
  const inUse = new Set(deployment.behaviors.map((b) => b.contextId));
  const unusedContexts = deployment.contexts.filter(
    (c) => !inUse.has(c.context_id),
  ).length;
  /* who uses each context, so a row can say when its instructions are shared */
  const byContext = new Map<string, BehaviorView[]>();
  for (const b of deployment.behaviors) {
    if (!b.contextId) continue;
    const list = byContext.get(b.contextId);
    if (list) list.push(b);
    else byContext.set(b.contextId, [b]);
  }
  const contextIds = new Set(deployment.contexts.map((c) => c.context_id));
  const line = (b: BehaviorView) => {
    const readiness = behaviorReadiness(deployment, b.behaviorId);
    if (!readiness.ready) return `unavailable: ${readiness.reason}`;
    const e = deployment.behaviorEnvironments.find(
      (x) => x.behaviorId === b.behaviorId,
    );
    const sharing = (b.contextId ? (byContext.get(b.contextId) ?? []) : []).filter(
      (x) => x.behaviorId !== b.behaviorId,
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
            <ArrowLeft className="size-3.5" /> Behaviors
          </button>
        </div>
        <BehaviorEditor
          key={draft.behaviorId}
          deployment={deployment}
          behavior={draft}
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
        item={behaviorId}
        toolbar={(id) => {
          const b = agentOf(deployment, id);
          return b ? (
            <RowControls deployment={deployment} behavior={b} inEditor />
          ) : null;
        }}
        /* the default is pinned first and named beside its title */
        rows={[...deployment.behaviors]
          .sort((a, b) => Number(b.isDefault) - Number(a.isDefault))
          .map((b) => ({
            tags: b.tags,
            id: b.behaviorId,
            title: b.displayName,
            titleNote: b.isDefault ? "Default" : undefined,
            meta: line(b),
            metaMono: true,
            trailing: <RowControls deployment={deployment} behavior={b} />,
            icon: (
              <BehaviorAvatar
                name={b.displayName}
                className="size-6 border-0 bg-transparent text-[10px]"
              />
            ),
          }))}
        createLabel="New behavior"
        empty="No behaviors yet. A behavior is what an agent is told, what it may use, and what runs it."
        onCreate={() => setDraft(newBehaviorView(deployment))}
        detail={(id) => {
          const behavior = agentOf(deployment, id)!;
          return (
            <BehaviorEditor
              key={behavior.behaviorId}
              deployment={deployment}
              behavior={behavior}
            />
          );
        }}
      />
      {!behaviorId && unusedContexts > 0 && (
        <p data-testid="unused-contexts" className="mt-3 text-sm text-muted-foreground">
          {unusedContexts === 1
            ? "1 context isn’t used by any behavior"
            : `${unusedContexts} contexts aren’t used by any behavior`}{" "}
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
