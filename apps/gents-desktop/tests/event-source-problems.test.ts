import { describe, expect, it } from "vitest";

import { eventSourceProblems } from "../src/ui/screens/agent/EventSourcesPanel";

const draft = (over: Record<string, string> = {}) => ({
  displayName: "",
  sourceCollection: "Order",
  eventKind: "",
  filter: "",
  correlationField: "",
  expectedCount: "",
  expectedCountField: "",
  timeoutSecs: "",
  minCount: "",
  workspaceAuthority: "",
  tags: [],
  ...over,
});

describe("an event source draft's grouping rules", () => {
  it("asks a grouped source for its correlation field there", () => {
    expect(eventSourceProblems(draft({ expectedCount: "2" })).correlationField).toBe(
      "Grouped events require a correlation field",
    );
  });

  it("takes a fixed count or a source field, said at the source field", () => {
    const problems = eventSourceProblems(
      draft({
        correlationField: "orderId",
        expectedCount: "2",
        expectedCountField: "n",
      }),
    );
    expect(problems.expectedCountField).toBe(
      "Choose a fixed expected count or a source field, not both",
    );
  });

  it("needs a count or a timeout once grouped, and a minimum within the count", () => {
    expect(
      eventSourceProblems(draft({ correlationField: "orderId", minCount: "2" }))
        .expectedCount,
    ).toBe("Grouped events require an expected count or timeout");
    expect(
      eventSourceProblems(
        draft({ correlationField: "orderId", expectedCount: "2", minCount: "3" }),
      ).minCount,
    ).toBe("Minimum count cannot exceed expected count");
  });

  it("has nothing to say about a valid ungrouped source", () => {
    expect(Object.values(eventSourceProblems(draft())).filter(Boolean)).toEqual([]);
  });
});
