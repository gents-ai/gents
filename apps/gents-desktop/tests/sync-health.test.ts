import { describe, expect, it } from "vitest";

import {
  projectSyncOperationalStatus,
  syncHealthDiagnostics,
  syncHealthState,
  type SyncHealthView,
} from "@source-inc/gents-desktop-client";

function health(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",
    cause: null,
    nextAction: null,
    lastError: null,
    runtimeCount: 1,
    connectedRuntimeCount: 1,
    activationErrorCount: 0,
    ...overrides,
  };
}

describe("projectSyncOperationalStatus", () => {
  it("names unobserved, healthy, and each cause the bridge derives", () => {
    expect(projectSyncOperationalStatus(null).shortLabel).toBe("Checking connection");
    expect(projectSyncOperationalStatus(health()).shortLabel).toBe("Connected");
    const reconnecting = projectSyncOperationalStatus(
      health({ state: "waiting", cause: "runtime_disconnected", nextAction: "wait" }),
    );
    expect(reconnecting.kind).toBe("waiting");
    expect(reconnecting.shortLabel).toBe("Reconnecting");
    const offline = projectSyncOperationalStatus(
      health({
        state: "waiting",
        cause: "discovery_offline",
        nextAction: "wait",
        lastError: "probe failed",
      }),
    );
    expect(offline.kind).toBe("waiting");
    expect(offline.reason).toBe("discovery_offline");
    expect(offline.detail).toBe("probe failed");
    expect(syncHealthState(null)).toBeNull();
    expect(syncHealthState(health({ state: "future-state" }))).toBeNull();
  });
});

describe("syncHealthDiagnostics", () => {
  it("exposes the cause, next action and the facts' counts", () => {
    const diagnostics = syncHealthDiagnostics(
      health({
        state: "blocked",
        cause: "enrollment_authentication",
        nextAction: "enroll_again",
        connectedRuntimeCount: 0,
        activationErrorCount: 1,
      }),
    );
    expect(diagnostics).toEqual({
      state: "blocked",
      cause: "enrollment_authentication",
      nextAction: "enroll_again",
      lastError: null,
      runtimeCount: 1,
      connectedRuntimeCount: 0,
      activationErrorCount: 1,
    });
  });
});
