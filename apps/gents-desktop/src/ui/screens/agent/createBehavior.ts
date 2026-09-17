import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";

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
  });
  navigate({
    name: "agent",
    agentDid: deployment.agentDid,
    section: "behaviors",
    item: behaviorId,
  });
}
