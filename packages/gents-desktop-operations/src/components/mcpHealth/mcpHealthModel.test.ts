import { describe, expect, it } from "vitest";

import type { MCPServiceHealthView } from "@source-inc/gents-desktop-client";
import { matchesFilter, projectStatus, visualState } from "./mcpHealthModel.js";

function service(
  overrides: Partial<MCPServiceHealthView> = {},
): MCPServiceHealthView {
  return {
    serviceId: "svc-1",
    status: "healthy",
    displayState: "healthy",
    failureCount: 0,
    kMax: 3,
    lastSeen: new Date().toISOString(),
    ...overrides,
  } as MCPServiceHealthView;
}

describe("mcpHealthModel", () => {
  it("visualState mirrors the raw persisted status, normalizing legacy stale", () => {
    expect(visualState(service({ status: "stale" }))).toBe("degraded");
    expect(visualState(service({ status: "evicted" }))).toBe("evicted");
    expect(visualState(service({ status: "reconnecting" }))).toBe(
      "reconnecting",
    );
    expect(visualState(service({ status: null }))).toBe("unknown");
  });

  it("projectStatus is a pass-through of the server-projected displayState", () => {
    expect(projectStatus(service({ displayState: "healthy" }))).toBe(
      "healthy",
    );
    expect(projectStatus(service({ displayState: "stale" }))).toBe("stale");
    expect(projectStatus(service({ displayState: "unreachable" }))).toBe(
      "unreachable",
    );
  });

  it("projectStatus falls back to unknown for a missing/unrecognized displayState", () => {
    expect(
      projectStatus(service({ displayState: undefined as unknown as string })),
    ).toBe("unknown");
    expect(
      projectStatus(service({ displayState: "bogus" as unknown as string })),
    ).toBe("unknown");
  });

  it("matches reconnecting filter only for reconnecting raw status", () => {
    expect(
      matchesFilter(service({ status: "reconnecting" }), "reconnecting"),
    ).toBe(true);
    expect(matchesFilter(service({ status: "evicted" }), "reconnecting")).toBe(
      false,
    );
  });

  it("matches unhealthy filter off the projected displayState, not the raw status", () => {
    expect(
      matchesFilter(
        service({ status: "evicted", displayState: "unreachable" }),
        "unhealthy",
      ),
    ).toBe(true);
    expect(
      matchesFilter(
        service({ status: "healthy", displayState: "healthy" }),
        "unhealthy",
      ),
    ).toBe(false);
  });
});
