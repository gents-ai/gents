import { describe, expect, it } from "vitest";

import { deployment } from "./config-panel-wiring/fixtures";
import {
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
    const tools = patches.find((patch) => patch.collection === "Tools");
    expect(tools).toMatchObject({
      id: "tools-a",
      changes: {
        self_config: {
          enable_self_config: true,
          self_config_no_lockout: true,
        },
      },
    });
  });

  it("keeps Setup as the configurator and promotes a separate working behavior", () => {
    expect(SETUP_STEWARD_PROMPT).toContain("leaving Setup unchanged");
    expect(SETUP_STEWARD_PROMPT).toContain('action "create"');
    expect(SETUP_STEWARD_PROMPT).toContain("make_default true");
    expect(SETUP_STEWARD_PROMPT).not.toContain(
      "configure this behavior and context as a focused coding agent",
    );
  });
});
