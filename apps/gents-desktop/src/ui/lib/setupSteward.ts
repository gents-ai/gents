/* First-run Setup behavior: one default behavior whose tools are the
   self-config family (get_my_config + configure_behavior/tools/profile). */
import type {
  ConfigComponentPatch,
  DeploymentView,
} from "@source-inc/gents-desktop-client";

export const SETUP_STEWARD_PROMPT = `You are the first-run setup steward for Gents, a local agent runtime. Your job is to help this user get a working agent for the work they actually want to do.

You have self-configuration tools: get_my_config to inspect the current setup, and configure_behavior, configure_tools, and configure_profile to change it. Committed changes apply to later requests, not this turn.

Start by asking what they want to do — coding in a specific repo, research, operations, or just chatting. Then:
1. Call get_my_config before changing anything.
2. Walk them through the smallest changes that fit that work: a behavior, tool permissions (files, bash), and the workspace root.
3. Explain each change in plain language before you apply it.
4. Never disable your own self-config tools.
5. You can read local files to inspect a repo they name. You cannot write files or run write-capable shell until they ask you to grant those tools.

For coding work, configure this behavior and context as a focused coding agent, set Tools.host.root to the exact absolute repo path, select ReadWrite files and Unrestricted bash with workspace_write execution on macOS, and keep self_config enabled. Tell the user the committed configuration applies starting with their next request, then use that next request to test the configured behavior.

If they just want to talk, stay on this Setup behavior and help from here.`;

export const SETUP_STEWARD_DESCRIPTION =
  "Walks you through configuring Gents for the work you want to do.";

export function setupStewardPatches(
  deployment: DeploymentView,
): ConfigComponentPatch[] {
  const behavior =
    deployment.behaviors.find((row) => row.isDefault) ?? deployment.behaviors[0];
  if (!behavior) return [];
  const context = deployment.contexts.find(
    (row) => row.context_id === behavior.contextId,
  );
  const toolsId = context?.tools_id;
  const patches: ConfigComponentPatch[] = [
    {
      collection: "AgentBehavior",
      id: behavior.behaviorId,
      changes: {
        display_name: "Setup",
        description: SETUP_STEWARD_DESCRIPTION,
      },
    },
  ];
  if (context) {
    patches.push({
      collection: "AgentContext",
      id: context.context_id,
      changes: {
        display_name: "Setup",
        system_prompt: SETUP_STEWARD_PROMPT,
      },
    });
  }
  if (toolsId) {
    patches.push({
      collection: "Tools",
      id: toolsId,
      changes: {
        self_config: {
          enable_self_config: true,
          self_config_no_lockout: true,
          self_config_dry_run: true,
        },
      },
    });
  }
  return patches;
}
