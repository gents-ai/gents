import { describe, expect, it } from "vitest";

import type { NodeView } from "../src/hooks/fleetStore";
import { agentOf, defaultAgentOf } from "../src/ui/lib/agents";

const node = (nodeDefault: string | null, marked: string) =>
  ({
    nodeDid: "node",
    node: { defaultAgentId: nodeDefault },
    agents: [
      { agentId: "coding", enabled: true, isDefault: marked === "coding" },
      { agentId: "setup", enabled: true, isDefault: marked === "setup" },
    ],
  }) as unknown as NodeView;

describe("a node's agents", () => {
  it("finds one by id", () => {
    expect(agentOf(node(null, "coding"), "setup")?.agentId).toBe("setup");
    expect(agentOf(node(null, "coding"), "missing")).toBeNull();
    expect(agentOf(null, "setup")).toBeNull();
  });

  /* every screen that preselects an agent uses the client's one rule */
  it("takes the node's default over the one marked default", () => {
    expect(defaultAgentOf(node("coding", "setup"))?.agentId).toBe("coding");
    expect(defaultAgentOf(node(null, "setup"))?.agentId).toBe("setup");
  });
});
