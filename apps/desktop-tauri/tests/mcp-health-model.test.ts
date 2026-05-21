import { describe, expect, it } from "vitest";

import type { MCPServiceHealthView } from "../src/lib/types";
import {
  matchesFilter,
  projectStatus,
  visualState,
} from "../src/components/mcpHealth/mcpHealthModel";

function service(overrides: Partial<MCPServiceHealthView> = {}): MCPServiceHealthView {
  return {
    serviceId: "svc-1",
    status: "healthy",
    failureCount: 0,
    kMax: 3,
    lastSeen: new Date().toISOString(),
    ...overrides,
  };
}

describe("mcpHealthModel", () => {
  it("projects stale rows into degraded visual state", () => {
    const visual = visualState(service({ status: "stale" }));

    expect(visual).toBe("degraded");
    expect(projectStatus(visual)).toBe("stale");
  });

  it("marks evicted services as stuck when failure count exceeds two K", () => {
    const visual = visualState(
      service({ status: "evicted", failureCount: 6, kMax: 3 }),
    );

    expect(visual).toBe("stuck");
    expect(projectStatus(visual)).toBe("unreachable");
  });

  it("matches reconnecting filter only for reconnecting visual state", () => {
    expect(matchesFilter(service({ status: "reconnecting" }), "reconnecting")).toBe(
      true,
    );
    expect(matchesFilter(service({ status: "evicted" }), "reconnecting")).toBe(false);
  });
});
