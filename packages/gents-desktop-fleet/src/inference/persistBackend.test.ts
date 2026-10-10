import { describe, expect, it, vi } from "vitest";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import {
  persistInferenceBackend,
  type PersistBackendOptions,
} from "./persistBackend.js";
import { resolveTargets } from "./resolveTargets.js";

// Only facts read by the setup controller; the full snapshot remains the bridge's projection.
function deployment(): DeploymentView {
  return {
    nodeDid: "did:test:owner",
    node: { defaultAgentId: "agent" },
    agents: [
      {
        agentId: "agent",
        inferenceProfileId: " profile ",
        backendId: "retired-flat-binding",
      },
    ],
    inferenceProfiles: [
      { profile_id: " profile ", backend_id: " backend ", model_name: "old" },
    ],
    inferenceBackends: [
      { backendId: " backend ", name: "Backend", models: ["advertised"] },
    ],
  } as unknown as DeploymentView;
}
const options: PersistBackendOptions = {
  name: "Configured",
  providerKind: "ChatGptCodex",
  endpoint: "https://example.test/v1",
  modelName: "selected",
  auth: { kind: "node_oauth" },
};
describe("canonical inference setup", () => {
  it("resolves agent through the profile and patches connection plus model atomically", async () => {
    const onPatchConfigComponents = vi.fn().mockResolvedValue(undefined);
    await persistInferenceBackend({
      deployment: deployment(),
      options,
      onPatchConfigComponents,
    });
    expect(onPatchConfigComponents).toHaveBeenCalledTimes(1);
    expect(onPatchConfigComponents).toHaveBeenCalledWith({
      nodeDid: "did:test:owner",
      patches: [
        {
          collection: "InferenceBackend",
          id: " backend ",
          changes: {
            name: "Configured",
            provider_kind: "ChatGptCodex",
            endpoint: "https://example.test/v1",
            auth: { kind: "node_oauth" },
            openai_wire_api: null,
            enabled: true,
          },
        },
        {
          collection: "InferenceProfile",
          id: " profile ",
          changes: { model_name: "selected" },
        },
      ],
    });
  });
  it("rejects missing canonical links without writing an inferred default", async () => {
    const snapshot = deployment();
    snapshot.inferenceProfiles = [];
    const onPatchConfigComponents = vi.fn();
    expect(resolveTargets(snapshot).error).toContain("inference profile");
    await expect(
      persistInferenceBackend({
        deployment: snapshot,
        options,
        onPatchConfigComponents,
      }),
    ).rejects.toThrow("inference profile");
    expect(onPatchConfigComponents).not.toHaveBeenCalled();
  });
  it("surfaces atomic admission failure without a second independent save", async () => {
    const onPatchConfigComponents = vi
      .fn()
      .mockRejectedValue(new Error("profile validation failed"));
    await expect(
      persistInferenceBackend({
        deployment: deployment(),
        options,
        onPatchConfigComponents,
      }),
    ).rejects.toThrow("profile validation failed");
    expect(onPatchConfigComponents).toHaveBeenCalledTimes(1);
  });
});
