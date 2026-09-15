import { describe, expect, it } from "vitest";
import type {
  InferenceDiscoveryResult,
  InferenceModelRecommendation,
} from "@source-inc/gents-desktop-client";
import { buildInferenceSetupPlan } from "../src/ui/lib/inferenceSetupPersistence";
import { deployment as fixtureDeployment } from "./config-panel-wiring/fixtures";

const recommendation: InferenceModelRecommendation = {
  defaultsVersion: "2026-09-14.1",
  summary: "Fixture defaults",
  contextWindow: null,
  maxOutputTokens: null,
  temperature: { recommended: 1, min: 0, max: 2, step: 0.05 },
  topP: { recommended: 0.95, min: 0, max: 1, step: 0.05 },
  reasoningEffort: null,
  maxConcurrent: { recommended: 1, min: 1, max: null },
};

const discovery: InferenceDiscoveryResult = {
  requestKey: "1:local",
  contractVersion: 1,
  defaultsVersion: "2026-09-14.1",
  requestedEndpoint: "http://workstation-1:8000/v1",
  effectiveEndpoint: "http://workstation-1:8000/v1",
  backendName: "Local server",
  providerKind: "OpenAiCompatible",
  openaiWireApi: "chat_completions",
  reachable: true,
  models: [],
  failure: null,
  manualEntryAllowed: false,
};

describe("inference setup persistence", () => {
  it("adds a separate backend/profile without replacing existing config or binding behaviors", () => {
    const deployment = structuredClone(fixtureDeployment);
    const before = structuredClone(deployment);
    const existing = deployment.inferenceBackends[0]!;
    const plan = buildInferenceSetupPlan({
      deployment,
      purpose: "add-backend",
      provider: "local",
      apiKey: "",
      oauth: false,
      discovery: {
        ...discovery,
        providerKind: existing.providerKind!,
        effectiveEndpoint: existing.endpoint!,
      },
      model: "new-model",
      recommendation,
      settings: {
        contextWindow: "",
        maxOutputTokens: "",
        temperature: "1",
        topP: "0.95",
        reasoningEffort: "",
        maxConcurrent: "8",
      },
    });
    expect(plan.document.agent_behaviors).toBeUndefined();
    expect(deployment.inferenceBackends.map((b) => b.backendId)).not.toContain(
      plan.document.inference_backends![0]!.backend_id,
    );
    expect(deployment.inferenceProfiles.map((p) => p.profile_id)).not.toContain(
      plan.profileId,
    );
    expect(plan.document.inference_backends![0]!.max_concurrent).toBe(8);
    expect(deployment).toEqual(before);
  });

  it.each([272000, 500000, 872000])(
    "binds Codex and preserves the selected context window %s",
    (contextWindow) => {
      const deployment = structuredClone(fixtureDeployment);
      deployment.inferenceBackends = [
        { ...deployment.inferenceBackends[0]!, enabled: false },
      ];
      deployment.inferenceProfiles = [deployment.inferenceProfiles[0]!];
      const plan = buildInferenceSetupPlan({
        deployment,
        provider: "openai",
        apiKey: "",
        oauth: true,
        discovery: {
          ...discovery,
          providerKind: "ChatGptCodex",
          effectiveEndpoint: "https://chatgpt.com/backend-api/codex",
          openaiWireApi: "responses",
        },
        model: "gpt-5.6-sol",
        recommendation: {
          ...recommendation,
          temperature: null,
          topP: null,
          contextWindow: { recommended: 272000, min: 1, max: 872000 },
          reasoningEffort: {
            recommended: "medium",
            choices: ["low", "medium", "high"],
          },
        },
        settings: {
          contextWindow: String(contextWindow),
          maxOutputTokens: "",
          temperature: "",
          topP: "",
          reasoningEffort: "high",
          maxConcurrent: "1",
        },
      });
      const backend = plan.document.inference_backends![0]!;
      const profile = plan.document.inference_profiles![0]!;
      expect(backend).toMatchObject({
        provider_kind: "ChatGptCodex",
        auth: { kind: "principal_oauth" },
        enabled: true,
      });
      expect(profile).toMatchObject({
        backend_id: backend.backend_id,
        model_name: "gpt-5.6-sol",
        context_window: contextWindow,
        reasoning_effort: "high",
        sampling_id: null,
      });
      expect(
        plan.document.agent_behaviors?.find(
          (row) => row.behavior_id === plan.defaultBehaviorId,
        )?.inference_profile_id,
      ).toBe(profile.profile_id);
      expect(plan.document.inference_sampling).toBeUndefined();
    },
  );

  it("preserves a configured compatible backend when adding another endpoint", () => {
    const deployment = structuredClone(fixtureDeployment);
    deployment.inferenceBackends = [
      {
        ...deployment.inferenceBackends[0]!,
        backendId: "local",
        providerKind: "OpenAiCompatible",
        endpoint: "http://another-server:8000/v1",
        apiKeyConfigured: true,
      },
    ];
    const plan = buildInferenceSetupPlan({
      deployment,
      provider: "local",
      apiKey: "",
      oauth: false,
      discovery,
      model: "GLM-5.3-Flash-NVFP4",
      recommendation,
      settings: {
        contextWindow: "",
        maxOutputTokens: "",
        temperature: "1",
        topP: "0.95",
        reasoningEffort: "",
        maxConcurrent: "1",
      },
    });
    expect(plan.document.inference_backends?.[0]?.backend_id).toBe("local-2");
    expect(plan.document.inference_profiles?.[0]?.backend_id).toBe("local-2");
    expect(plan.profileId).toBe("profile-local-2");
  });

  it("plans backend, exact model defaults, and Setup behavior in one document", () => {
    const deployment = structuredClone(fixtureDeployment);
    deployment.inferenceBackends = [
      {
        ...deployment.inferenceBackends[0]!,
        backendId: `${deployment.agentDid}:backend`,
        apiKeyConfigured: false,
        authKind: null,
        probeStatus: null,
      },
    ];
    deployment.inferenceProfiles = [
      {
        ...deployment.inferenceProfiles[0]!,
        backend_id: `${deployment.agentDid}:backend`,
      },
    ];
    deployment.behaviorConfigs = [deployment.behaviorConfigs[0]!];

    const plan = buildInferenceSetupPlan({
      deployment,
      provider: "local",
      apiKey: "",
      oauth: false,
      discovery,
      model: "GLM-5.3-Flash-NVFP4",
      recommendation,
      settings: {
        contextWindow: "",
        maxOutputTokens: "",
        temperature: "1",
        topP: "0.95",
        reasoningEffort: "",
        maxConcurrent: "1",
      },
    });

    expect(plan.document.inference_backends).toEqual([
      expect.objectContaining({
        endpoint: "http://workstation-1:8000/v1",
        auth: { kind: "unauthenticated" },
      }),
    ]);
    expect(plan.document.inference_profiles).toEqual([
      expect.objectContaining({ model_name: "GLM-5.3-Flash-NVFP4" }),
    ]);
    expect(plan.document.inference_sampling).toEqual([
      expect.objectContaining({ temperature: 1, top_p: 0.95 }),
    ]);
    expect(plan.document.agent_behaviors).toEqual([
      expect.objectContaining({
        behavior_id: "default",
        context_id: "context-a",
        inference_profile_id: plan.profileId,
      }),
    ]);
  });
});
