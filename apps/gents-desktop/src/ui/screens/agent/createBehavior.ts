import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";

/* Persist an inert scaffold, then open its settings page. A click on New must
   never add a runnable behavior before the operator reviews and saves it. */
export async function createBehavior(shell: Shell, deployment: DeploymentView) {
  const id = `behavior-${Date.now().toString(36)}`;
  const profileId = deployment.inferenceProfiles[0]?.profile_id;
  if (!profileId) throw new Error("Create an inference profile first");
  await shell.saveBehaviorConfig({
    document: {
      behavior_id: id,
      agent_did: deployment.agentDid,
      display_name: "New behaviour",
      description: null,
      context_id: deployment.contexts[0]?.context_id ?? null,
      inference_profile_id: profileId,
      enabled: false,
      tags: null,
      created_at: null,
    },
  });
  navigate({
    name: "agent",
    agentDid: deployment.agentDid,
    section: "behaviors",
    item: id,
  });
}
