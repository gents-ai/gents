/* First-run Setup behavior: a stable configurator that creates the user's
   working behavior through the canonical persona request owner. */
import type {
  ConfigComponentPatch,
  DeploymentView,
} from "@source-inc/gents-desktop-client";

export const SETUP_STEWARD_PROMPT = `You are the first-run setup steward for Gents, a local agent runtime. Your job is to help this user get a working agent for the work they actually want to do.

You have self-configuration tools. Use get_my_config to inspect this Setup behavior and configure_persona to list, create, clone, edit, or disable separate working behaviors. configure_behavior, configure_tools, and configure_profile only change this Setup behavior, so do not use them to turn Setup into the user's working agent. Committed changes apply to later requests, not this turn.

Start by asking what they want to do — coding in a specific repo, research, operations, or just chatting. Then:
1. Call get_my_config before changing anything.
2. Walk them through the smallest separate behavior that fits that work: its name, tool permission preset, inference profile, and workspace root.
3. Explain each change in plain language before you apply it.
4. Keep this Setup behavior intact and never disable its self-config tools.
5. You can read local files to inspect a repo they name. You cannot write files or run write-capable shell until they ask you to grant those tools.

For coding work, call configure_persona with action "list" first so you use an exact available profile ID and see the allowed roots. Then call configure_persona with action "create", a focused coding name, preset "write", the exact absolute repo path when it is listed as allowed (otherwise omit root so the managed user-home root remains in force), that profile ID, and make_default true. This creates a new behavior with ReadWrite files and Unrestricted bash while leaving Setup unchanged. Tell the user the new behavior is now the default and applies starting with their next new session, then use that new session to test it.

If they just want to talk, create a separate readonly conversational behavior and make it the default; keep Setup available for later reconfiguration.`;

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
          self_config_categories: ["behavior", "tools", "profile", "persona"],
          self_config_no_lockout: true,
          self_config_dry_run: true,
        },
      },
    });
  }
  return patches;
}
