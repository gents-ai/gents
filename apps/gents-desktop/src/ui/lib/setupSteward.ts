/* First-run Engineer behavior: a stable configurator that creates the user's
   working behavior through the canonical persona request owner. */
import type {
  ConfigComponentPatch,
  DeploymentView,
} from "@source-inc/gents-desktop-client";

import sharedSetupPrompt from "../../../../../crates/gents-protocol/prompts/setup.md?raw";
import setupSelfConfig from "../../../../../crates/gents-protocol/presets/setup-self-config.json";

export const SETUP_STEWARD_PROMPT = sharedSetupPrompt;

export const SETUP_STEWARD_DESCRIPTION =
  "Walks you through configuring Gents for the work you want to do.";
export const SETUP_STEWARD_BEHAVIOR_TAG = "gents:setup-steward";

export function setupStewardPatches(
  deployment: DeploymentView,
): ConfigComponentPatch[] {
  const behavior =
    deployment.behaviors.find((row) => row.tags.includes(SETUP_STEWARD_BEHAVIOR_TAG)) ??
    deployment.behaviors.find((row) => row.isDefault) ??
    deployment.behaviors[0];
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
        display_name: "The Engineer",
        description: SETUP_STEWARD_DESCRIPTION,
        tags: Array.from(
          new Set([...(behavior.tags ?? []), SETUP_STEWARD_BEHAVIOR_TAG]),
        ),
      },
    },
  ];
  if (context) {
    patches.push({
      collection: "AgentContext",
      id: context.context_id,
      changes: {
        display_name: "The Engineer",
        system_prompt: SETUP_STEWARD_PROMPT,
      },
    });
  }
  if (toolsId) {
    patches.push({
      collection: "Tools",
      id: toolsId,
      changes: {
        self_config: structuredClone(setupSelfConfig),
        built_ins: {
          ...deployment.tools.find((tools) => tools.tools_id === toolsId)?.built_ins,
          enable_graph_tools: true,
        },
      },
    });
  }
  return patches;
}
