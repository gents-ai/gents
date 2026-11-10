import { describe, expect, it } from "vitest";

import { backendIsConfigured } from "./firstRun.js";
import type { InferenceBackendView } from "./generated/InferenceBackendView.js";

function backend(overrides: Partial<InferenceBackendView>): InferenceBackendView {
  return {
    backendId: "backend",
    name: null,
    providerKind: "ClaudeCliSubscription",
    openaiWireApi: null,
    endpoint: null,
    authKind: null,
    connectTimeoutSecs: null,
    discoveryTimeoutSecs: null,
    apiKeyConfigured: false,
    apiKeyEnvVar: null,
    maxConcurrent: null,
    maxQueueDepth: null,
    enabled: true,
    tags: [],
    models: [],
    advertisedModels: [],
    probeStatus: null,
    accountRef: null,
    ...overrides,
  };
}

describe("backendIsConfigured", () => {
  it("counts node OAuth, account pools and environment keys as configured", () => {
    for (const authKind of ["node_oauth", "node_oauth_pool", "environment"]) {
      expect(backendIsConfigured(backend({ authKind }))).toBe(true);
    }
  });

  it("needs a healthy probe for an unauthenticated backend", () => {
    expect(backendIsConfigured(backend({ authKind: "unauthenticated" }))).toBe(false);
    expect(
      backendIsConfigured(backend({ authKind: "unauthenticated", probeStatus: "ok" })),
    ).toBe(true);
  });

  it("never counts a disabled backend", () => {
    expect(backendIsConfigured(backend({ authKind: "node_oauth_pool", enabled: false }))).toBe(
      false,
    );
  });
});
