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

  it("routes provider discovery and both recommendations through the bridge", async () => {
    const postJson = vi.fn().mockResolvedValue({ models: [] });
    const adapter = createBridgeHttpAdapter({ getJson: vi.fn(), postJson });
    const request = { provider: "local", modelName: "test-model" };

    await adapter.discoverInferenceModels(
      request as unknown as Parameters<typeof adapter.discoverInferenceModels>[0],
    );
    await adapter.getInferenceModelRecommendation(
      request as unknown as Parameters<
        typeof adapter.getInferenceModelRecommendation
      >[0],
    );
    await adapter.getInferenceBackendRecommendation(
      request as unknown as Parameters<
        typeof adapter.getInferenceBackendRecommendation
      >[0],
    );

    expect(postJson).toHaveBeenNthCalledWith(
      1,
      "/desktop/inference/models/discover",
      request,
    );
    expect(postJson).toHaveBeenNthCalledWith(
      2,
      "/desktop/inference/model/recommendation",
      request,
    );
    expect(postJson).toHaveBeenNthCalledWith(
      3,
      "/desktop/inference/backend/recommendation",
      request,
    );
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

  it("routes existing component edits through canonical patch", async () => {
    const postJson = vi.fn().mockResolvedValue({ bootstrap: {}, client: null });
    const adapter = createBridgeHttpAdapter({ getJson: vi.fn(), postJson });
    const request = {
      agentDid: "did:test:agent",
      patches: [{ collection: "AgentContext" as const, id: "context-a", changes: {} }],
    };
    await adapter.patchConfigComponents(request);
    expect(postJson).toHaveBeenCalledWith("/desktop/config/components/patch", request);
  });

  it("routes event source saves through the bridge command", async () => {
    const postJson = vi.fn().mockResolvedValue({ bootstrap: {}, client: null });
    const adapter = createBridgeHttpAdapter({ getJson: vi.fn(), postJson });
    const request = {
      document: {
        agent_did: "did:test:agent",
        event_source_id: "event-a",
        source_collection: "AgentRequest",
      },
    };
    await adapter.saveEventSourceConfig(request);
    expect(postJson).toHaveBeenCalledWith("/desktop/event-source/save", request);
  });
});
