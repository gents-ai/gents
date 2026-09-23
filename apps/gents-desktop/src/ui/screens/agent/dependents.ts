/* What a delete leaves behind, read from the loaded configuration. Every
   delete asks for the document's name; this says, in the same dialog, which
   other documents point at it and lose that reference. One owner for the
   wording, so the editor's Danger zone and a list row's menu agree. */
import type { DeploymentView } from "@source-inc/gents-desktop-client";

export type DependentKind =
  | "behavior"
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

const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/* who points at the document, as "N things use it" parts */
export function dependents(
  deployment: DeploymentView,
  kind: DependentKind,
  id: string,
): string[] {
  const parts: string[] = [];
  const add = (n: number, one: string, many: string) => {
    if (n > 0) parts.push(count(n, one, many));
  };
  switch (kind) {
    case "behavior":
      if (deployment.agentPrincipal.defaultBehaviorId === id)
        parts.push("the agent's default behavior");
      add(deployment.tasks.filter((t) => t.behaviorId === id).length, "task", "tasks");
      add(
        deployment.subagentTargets.filter((t) => t.behavior_id === id).length,
        "subagent target",
        "subagent targets",
      );
      break;
    case "context":
      add(
        deployment.behaviors.filter((b) => b.contextId === id).length,
        "behavior",
        "behaviors",
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
        deployment.behaviors.filter((b) => b.inferenceProfileId === id).length,
        "behavior",
        "behaviors",
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

/* the sentence for the confirmation, or nothing when no document points at it */
export function dependentsWarning(
  deployment: DeploymentView,
  kind: DependentKind,
  id: string,
): string | undefined {
  const parts = dependents(deployment, kind, id);
  if (!parts.length) return undefined;
  return `Used by ${parts.join(" and ")}; they lose this reference.`;
}
