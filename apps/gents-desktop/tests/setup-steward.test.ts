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

  it("keeps Setup as the configurator and promotes a separate working behavior", () => {
    expect(SETUP_STEWARD_PROMPT).toContain("Keep Setup unchanged");
    expect(SETUP_STEWARD_PROMPT).toContain("config behavior create");
    expect(SETUP_STEWARD_PROMPT).toContain("--default");
    expect(SETUP_STEWARD_PROMPT).toContain("AgentSession selects a behavior");
    expect(SETUP_STEWARD_PROMPT).toContain("Unsafe or invalid request");
    expect(SETUP_STEWARD_PROMPT).toContain("Verify the result");
    expect(SETUP_STEWARD_PROMPT).toContain(
      "Keep template data separate from instructions and metadata",
    );
    expect(SETUP_STEWARD_PROMPT).toContain("minimal runnable result early");
    expect(SETUP_STEWARD_PROMPT).toContain("runtime's process ceiling and root");
    expect(SETUP_STEWARD_PROMPT).toContain("targeted config reads");
    expect(SETUP_STEWARD_PROMPT).toContain("run_graph");
    expect(SETUP_STEWARD_PROMPT).toContain(
      "Never infer its language or workflow from a directory name",
    );
    expect(SETUP_STEWARD_PROMPT).toContain("config pack install");
    expect(SETUP_STEWARD_PROMPT).toContain("--inference-slot NAME=PROFILE_ID");
    expect(SETUP_STEWARD_PROMPT).toContain("--digest DIGEST");
    expect(SETUP_STEWARD_PROMPT).toContain(
      "config tools preview --behavior BEHAVIOR_ID",
    );
    expect(SETUP_STEWARD_PROMPT).toContain("config tools edit --behavior BEHAVIOR_ID");
    expect(SETUP_STEWARD_PROMPT).toContain("enable_graph_tools=true");
    expect(SETUP_STEWARD_PROMPT).toContain('bash.network_mode="disabled"');
    expect(SETUP_STEWARD_PROMPT).toContain(
      "config behavior preview edit BEHAVIOR_ID --set FIELD=JSON",
    );
    expect(SETUP_STEWARD_PROMPT).toContain("config behavior default BEHAVIOR_ID");
    expect(SETUP_STEWARD_PROMPT).not.toContain("config behavior tools");
    expect(SETUP_STEWARD_PROMPT).not.toContain("behavior edit --id");
    expect(SETUP_STEWARD_PROMPT).not.toContain("get_my_config");
    expect(SETUP_STEWARD_PROMPT).not.toContain("configure_behaviors");
    expect(SETUP_STEWARD_PROMPT).not.toContain("install_pack");
    expect(SETUP_STEWARD_PROMPT).toContain("Starter recipes are optional");
    expect(SETUP_STEWARD_PROMPT).toContain(
      "A coding/review request is not runtime repair authority",
    );
    expect(SETUP_STEWARD_PROMPT).toContain("Never adopt another runtime home");
    expect(SETUP_STEWARD_PROMPT).toContain(
      "Graph terminal states are succeeded, failed and cancelled",
    );
    expect(SETUP_STEWARD_PROMPT).toContain("EventSource watches the input collection");
    expect(SETUP_STEWARD_PROMPT).toContain(
      "Skills supply procedures and declare dependencies; they never grant tools",
    );
    expect(SETUP_STEWARD_PROMPT).not.toContain(
      "configure this behavior and context as a focused coding agent",
    );
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
