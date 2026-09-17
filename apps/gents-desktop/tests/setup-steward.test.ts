import { describe, expect, it } from "vitest";

import { deployment } from "./config-panel-wiring/fixtures";
import {
  SETUP_STEWARD_BEHAVIOR_TAG,
  SETUP_STEWARD_PROMPT,
  setupStewardPatches,
} from "../src/ui/lib/setupSteward";

describe("setup steward patches", () => {
  it("wires the default behavior, context prompt, and self-config tools", () => {
    const withContext = {
      ...deployment,
      contexts: [
        {
          context_id: "context-a",
          agent_did: deployment.agentDid,
          display_name: "Default",
          description: null,
          system_prompt: "old",
          tools_id: "tools-a",
          compaction_id: null,
          skill_ids: [],
          tags: [],
        },
      ],
      tools: [
        {
          tools_id: "tools-a",
          agent_did: deployment.agentDid,
          display_name: "Standard",
          built_ins: { enable_context_budget: true },
          self_config: null,
        },
      ],
    };
    const patches = setupStewardPatches(withContext);
    expect(patches.map((patch) => patch.collection)).toEqual([
      "AgentBehavior",
      "AgentContext",
      "Tools",
    ]);
    expect(patches[0]).toMatchObject({
      changes: { tags: [SETUP_STEWARD_BEHAVIOR_TAG] },
    });
    const tools = patches.find((patch) => patch.collection === "Tools");
    expect(patches.find((patch) => patch.collection === "AgentContext")).toMatchObject({
      id: "context-a",
      changes: { system_prompt: SETUP_STEWARD_PROMPT },
    });
    expect(tools).toMatchObject({
      id: "tools-a",
      changes: {
        built_ins: { enable_graph_tools: true, enable_context_budget: true },
        self_config: {
          enable_self_config: true,
          self_config_categories: [
            "behavior",
            "tools",
            "profile",
            "persona",
            "backend",
            "mcp_service",
            "automation",
          ],
          self_config_no_lockout: true,
          enable_pack_install: true,
        },
      },
    });
  });

  it("patches the protected Setup behavior after a working behavior becomes default", () => {
    const configured = {
      ...deployment,
      agentPrincipal: {
        ...deployment.agentPrincipal,
        defaultBehaviorId: "ops",
      },
      behaviors: deployment.behaviors.map((behavior) => ({
        ...behavior,
        isDefault: behavior.behaviorId === "ops",
        tags: behavior.behaviorId === "default" ? [SETUP_STEWARD_BEHAVIOR_TAG] : [],
      })),
    };

    const patches = setupStewardPatches(configured);
    expect(patches[0]).toMatchObject({
      collection: "AgentBehavior",
      id: "default",
    });
    expect(patches).not.toContainEqual(
      expect.objectContaining({ collection: "AgentBehavior", id: "ops" }),
    );
  });
});
