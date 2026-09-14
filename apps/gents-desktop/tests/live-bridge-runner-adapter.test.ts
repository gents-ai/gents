import { describe, expect, it, vi } from "vitest";

import { createBridgeHttpAdapter } from "./live-bridge-runner/adapter";

describe("live bridge runner startup/config adapter", () => {
  it("loads the real inference catalog route", async () => {
    const catalog = { contractVersion: 1, defaultsVersion: "test", providers: [] };
    const getJson = vi.fn().mockResolvedValue(catalog);
    const adapter = createBridgeHttpAdapter({
      getJson,
      postJson: vi.fn(),
    });

    await expect(adapter.getInferenceSetupCatalog()).resolves.toBe(catalog);
    expect(getJson).toHaveBeenCalledWith("/desktop/inference/setup/catalog");
  });

  it("routes component packs through canonical apply", async () => {
    const snapshot = { bootstrap: {}, client: null };
    const postJson = vi.fn().mockResolvedValue(snapshot);
    const adapter = createBridgeHttpAdapter({
      getJson: vi.fn(),
      postJson,
    });
    const request = {
      document: {
        agent_principal: { agent_did: "did:test:agent" },
        contexts: [
          {
            agent_did: "did:test:agent",
            context_id: "context-a",
            name: "Context A",
          },
        ],
      },
    };

    await expect(adapter.applyConfigComponents(request)).resolves.toBe(snapshot);
    expect(postJson).toHaveBeenCalledWith("/desktop/config/components/apply", request);
  });
});
