import { describe, expect, it } from "vitest";

import type { NodeView } from "../src/hooks/fleetStore";
import { agentOf, defaultAgentOf } from "../src/ui/lib/agents";

const node = (principalDefault: string | null, marked: string) =>
  ({
    agentDid: "node",
    agentPrincipal: { defaultBehaviorId: principalDefault },
    behaviors: [
      { behaviorId: "coding", enabled: true, isDefault: marked === "coding" },
      { behaviorId: "setup", enabled: true, isDefault: marked === "setup" },
    ],
  }) as unknown as NodeView;

describe("a node's agents", () => {
  it("finds one by id", () => {
    expect(agentOf(node(null, "coding"), "setup")?.behaviorId).toBe("setup");
    expect(agentOf(node(null, "coding"), "missing")).toBeNull();
    expect(agentOf(null, "setup")).toBeNull();
  });

  /* every screen that preselects an agent uses the client's one rule */
  it("takes the principal's default over the one marked default", () => {
    expect(defaultAgentOf(node("coding", "setup"))?.behaviorId).toBe("coding");
    expect(defaultAgentOf(node(null, "setup"))?.behaviorId).toBe("setup");
  });
});
