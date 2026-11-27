import type { NodeView } from "../../../hooks/fleetStore";
import { href } from "@/lib/router";

export type DependentKind =
  | "agent"
  | "context"
  | "backend"
  | "profile"
  | "tools"
  | "tool-service"
  | "skill"
  | "task"
  | "schedule"
  | "event-source"
  | "trigger";

export type BlockingDependent = {
  key: string;
  label: string;
  href?: string;
};

export type DeleteBlocker = {
  summary: string;
  items: BlockingDependent[];
};

const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

export function dependents(
  deployment: NodeView,
  kind: DependentKind,
  id: string,
): string[] {
  const parts: string[] = [];
  const add = (n: number, one: string, many: string) => {
    if (n > 0) parts.push(count(n, one, many));
  };
  switch (kind) {
    case "agent":
      if (deployment.node.defaultAgentDid === id)
        parts.push("the node's default agent");
      add(deployment.tasks.filter((t) => t.agentDid === id).length, "task", "tasks");
      add(
        deployment.agentTargets.filter((t) => t.agent_did === id).length,
        "agent target",
        "agent targets",
      );
      break;
    case "context":
      add(
        deployment.agents.filter((b) => b.contextId === id).length,
        "agent",
        "agents",
      );
      break;
    case "backend":
      add(
        deployment.inferenceProfiles.filter((p) => p.backend_id === id).length,
        "inference profile",
        "inference profiles",
      );
      break;
    case "profile":
      add(
        deployment.agents.filter((b) => b.inferenceProfileId === id).length,
        "agent",
        "agents",
      );
      break;
    case "tools":
      add(
        deployment.contexts.filter((c) => c.tools_id === id).length,
        "context",
        "contexts",
      );
      break;
    case "tool-service":
      add(
        deployment.tools.filter((t) =>
          (t.remote?.services ?? []).some((s) => s.mcp_service_id === id),
        ).length,
        "Tools document",
        "Tools documents",
      );
      break;
    case "skill":
      add(
        deployment.contexts.filter((c) => c.skill_ids?.includes(id)).length,
        "context",
        "contexts",
      );
      break;
    case "task":
      add(
        deployment.triggers.filter((t) => t.config.task_id === id).length,
        "trigger",
        "triggers",
      );
      break;
    case "schedule":
      add(
        deployment.triggers.filter(
          (t) =>
            t.config.source.kind === "schedule" && t.config.source.schedule_id === id,
        ).length,
        "trigger",
        "triggers",
      );
      break;
    case "event-source":
      add(
        deployment.triggers.filter(
          (t) =>
            t.config.source.kind === "event" && t.config.source.event_source_id === id,
        ).length,
        "trigger",
        "triggers",
      );
      break;
    case "trigger":
      break;
  }
  return parts;
}

export function deleteBlocker(
  deployment: NodeView,
  kind: DependentKind,
  id: string,
): DeleteBlocker | undefined {
  const parts = dependents(deployment, kind, id);
  if (!parts.length) return undefined;
  const nodeDid = deployment.nodeDid;
  const link = (section: string, item: string) =>
    href({ name: "agent", nodeDid, section, item });
  const items: BlockingDependent[] = [];
  switch (kind) {
    case "agent":
      if (deployment.node.defaultAgentDid === id) {
        items.push({
          key: "default-agent",
          label: "Node default agent",
        });
      }
      for (const t of deployment.tasks.filter((t) => t.agentDid === id)) {
        items.push({
          key: `task:${t.taskId}`,
          label: t.name ?? t.taskId,
          href: link("tasks", t.taskId),
        });
      }
      for (const t of deployment.agentTargets.filter((t) => t.agent_did === id)) {
        items.push({
          key: `target:${t.target_id}`,
          label: t.name || t.target_id,
        });
      }
      break;
    case "context":
      for (const a of deployment.agents.filter((b) => b.contextId === id)) {
        items.push({
          key: `agent:${a.agentDid}`,
          label: a.displayName,
          href: link("agents", a.agentDid),
        });
      }
      break;
    case "backend":
      for (const p of deployment.inferenceProfiles.filter((p) => p.backend_id === id)) {
        items.push({
          key: `profile:${p.profile_id}`,
          label: p.display_name ?? p.profile_id,
          href: link("profiles", p.profile_id),
        });
      }
      break;
    case "profile":
      for (const a of deployment.agents.filter((b) => b.inferenceProfileId === id)) {
        items.push({
          key: `agent:${a.agentDid}`,
          label: a.displayName,
          href: link("agents", a.agentDid),
        });
      }
      break;
    case "tools":
      for (const c of deployment.contexts.filter((c) => c.tools_id === id)) {
        items.push({
          key: `context:${c.context_id}`,
          label: c.display_name ?? c.context_id,
          href: link("contexts", c.context_id),
        });
      }
      break;
    case "tool-service":
      for (const t of deployment.tools.filter((t) =>
        (t.remote?.services ?? []).some((s) => s.mcp_service_id === id),
      )) {
        items.push({
          key: `tools:${t.tools_id}`,
          label: t.display_name ?? t.tools_id,
          href: link("tools", t.tools_id),
        });
      }
      break;
    case "skill":
      for (const c of deployment.contexts.filter((c) => c.skill_ids?.includes(id))) {
        items.push({
          key: `context:${c.context_id}`,
          label: c.display_name ?? c.context_id,
          href: link("contexts", c.context_id),
        });
      }
      break;
    case "task":
      for (const t of deployment.triggers.filter((t) => t.config.task_id === id)) {
        items.push({
          key: `trigger:${t.config.trigger_id}`,
          label: t.config.display_name ?? t.config.trigger_id,
          href: link("triggers", t.config.trigger_id),
        });
      }
      break;
    case "schedule":
      for (const t of deployment.triggers.filter(
        (t) =>
          t.config.source.kind === "schedule" && t.config.source.schedule_id === id,
      )) {
        items.push({
          key: `trigger:${t.config.trigger_id}`,
          label: t.config.display_name ?? t.config.trigger_id,
          href: link("triggers", t.config.trigger_id),
        });
      }
      break;
    case "event-source":
      for (const t of deployment.triggers.filter(
        (t) =>
          t.config.source.kind === "event" && t.config.source.event_source_id === id,
      )) {
        items.push({
          key: `trigger:${t.config.trigger_id}`,
          label: t.config.display_name ?? t.config.trigger_id,
          href: link("triggers", t.config.trigger_id),
        });
      }
      break;
    case "trigger":
      break;
  }
  const pronoun = items.length === 1 ? "it" : "them";
  return {
    summary: `Still used by ${parts.join(" and ")}. Repoint or delete ${pronoun} first.`,
    items,
  };
}
