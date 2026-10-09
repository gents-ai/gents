import { describe, expect, it } from "vitest";

import { deployment } from "./config-panel-wiring/fixtures";
import {
  ENGINEER_AGENT_TAG,
  ENGINEER_PROMPT,
  engineerPatches,
} from "../src/ui/lib/setupSteward";

describe("engineer setup patches", () => {
  it("wires the default agent, context prompt, and self-config tools", () => {
    const withContext = {
      ...deployment,
      contexts: [
        {
          context_id: "context-a",
          node_did: deployment.nodeDid,
          display_name: "Default",
          description: null,
          system_prompt: "old",
          tools_id: "tools-a",
          compaction_id: null,
          skill_ids: null,
          tags: [],
        },
      ],
      tools: [
        {
          tools_id: "tools-a",
          node_did: deployment.nodeDid,
          display_name: "Standard",
          built_ins: { enable_context_budget: true },
        },
      ],
    };
    const patches = engineerPatches(withContext);
    expect(patches.map((patch) => patch.collection)).toEqual([
      "Agent",
      "AgentContext",
      "Tools",
    ]);
    expect(patches[0]).toMatchObject({
      changes: { tags: [ENGINEER_AGENT_TAG] },
    });
    const tools = patches.find((patch) => patch.collection === "Tools");
    expect(patches.find((patch) => patch.collection === "AgentContext")).toMatchObject({
      id: "context-a",
      changes: { system_prompt: ENGINEER_PROMPT },
    });
    expect(tools).toMatchObject({
      id: "tools-a",
      changes: {
        built_ins: { enable_graph_tools: true, enable_context_budget: true },
        self_config: {
          enable_self_config: true,
          self_config_categories: [
            "node",
            "agent",
            "tools",
            "profile",
            "backend",
            "mcp_service",
            "automation",
          ],
          self_config_no_lockout: true,
          self_config_preview: true,
          enable_pack_install: true,
        },
      },
    });
  });

  it("patches the protected Engineer agent after a working agent becomes default", () => {
    const configured = {
      ...deployment,
      node: {
        ...deployment.node,
        defaultAgentId: "ops",
      },
      agents: deployment.agents.map((agent) => ({
        ...agent,
        isDefault: agent.agentId === "ops",
        tags: agent.agentId === "default" ? [ENGINEER_AGENT_TAG] : [],
      })),
    };

    const patches = engineerPatches(configured);
    expect(patches[0]).toMatchObject({
      collection: "Agent",
      id: "default",
    });
    expect(patches).not.toContainEqual(
      expect.objectContaining({ collection: "Agent", id: "ops" }),
    );
  });
});
