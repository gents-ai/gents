import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";

const pendingScaffoldRequests = new Map<string, string>();

function scaffoldRequestId(agentDid: string) {
  const prior = pendingScaffoldRequests.get(agentDid);
  if (prior) return prior;
  const requestId =
    globalThis.crypto?.randomUUID?.() ??
    `scaffold-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  pendingScaffoldRequests.set(agentDid, requestId);
  return requestId;
}

/* Persist an inert scaffold, then open its settings page. A click on New must
   never add a runnable behavior before the operator reviews and saves it. */
export async function createBehavior(shell: Shell, deployment: DeploymentView) {
  const source =
    deployment.behaviors.find((behavior) => behavior.isDefault) ??
    deployment.behaviors[0];
  if (!source) throw new Error("Create a behavior to use as the scaffold source first");
  const behaviorId = await shell.createBehaviorScaffold({
    agentDid: deployment.agentDid,
    sourceBehaviorId: source.behaviorId,
    displayName: "New behaviour",
    requestId: scaffoldRequestId(deployment.agentDid),
  });
  pendingScaffoldRequests.delete(deployment.agentDid);
  navigate({
    name: "agent",
    agentDid: deployment.agentDid,
    section: "behaviors",
    item: behaviorId,
  });
}
