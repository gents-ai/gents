import { describe, expect, it } from "vitest";

import { deployment } from "./config-panel-wiring/fixtures";
import { ENGINEER_AGENT_TAG, engineerAgentOf } from "../src/ui/lib/engineer";

describe("engineerAgentOf", () => {
  it("picks the tagged Engineer over the default agent", () => {
    const tagged = {
      ...deployment,
      node: { ...deployment.node, defaultAgentDid: "ops" },
      agents: deployment.agents.map((agent) => ({
        ...agent,
        isDefault: agent.agentDid === "ops",
        tags: agent.agentDid === "default" ? [ENGINEER_AGENT_TAG] : [],
      })),
    };
    expect(engineerAgentOf(tagged)?.agentDid).toBe("default");
  });
});
