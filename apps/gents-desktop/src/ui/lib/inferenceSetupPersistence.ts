import type {
  DeploymentView,
  InferenceDiscoveryResult,
  InferenceModelRecommendation,
  InferenceProviderId,
  PackConfig,
  ReasoningEffort,
} from "@source-inc/gents-desktop-client";
import { backendIsConfigured, shouldRebindSetupDefault } from "./firstRun";

export type InferenceSetupSettings = {
  contextWindow: string;
  maxOutputTokens: string;
  temperature: string;
  topP: string;
  reasoningEffort: ReasoningEffort | "";
  maxConcurrent: string;
};

export function buildInferenceSetupPlan({
  deployment,
  provider,
  apiKey,
  oauth,
  discovery,
  model,
  recommendation,
  settings,
  purpose = "onboarding",
}: {
  deployment: DeploymentView;
  provider: InferenceProviderId;
  apiKey: string;
  oauth: boolean;
  discovery: InferenceDiscoveryResult;
  model: string;
  recommendation: InferenceModelRecommendation;
  settings: InferenceSetupSettings;
  purpose?: "onboarding" | "add-backend";
}): { document: PackConfig; profileId: string; defaultBehaviorId: string | null } {
  const addingBackend = purpose === "add-backend";
  const normalizedEndpoint = (endpoint: string | null) =>
    endpoint?.trim().replace(/\/+$/, "") ?? "";
  const sameConnection = deployment.inferenceBackends.find(
    (backend) =>
      backend.providerKind === discovery.providerKind &&
      normalizedEndpoint(backend.endpoint) ===
        normalizedEndpoint(discovery.effectiveEndpoint),
  );
  const placeholder = deployment.inferenceBackends.find(
    (backend) => !backendIsConfigured(backend),
  );
  const addingExtra = deployment.inferenceBackends.some(
    (backend) =>
      backendIsConfigured(backend) && backend.backendId !== sameConnection?.backendId,
  );
  const backendId =
    (!addingBackend ? sameConnection?.backendId : undefined) ??
    (!addingBackend && !addingExtra ? placeholder?.backendId : undefined) ??
    (() => {
      let candidate: string = provider;
      let suffix = 2;
      while (
        deployment.inferenceBackends.some((backend) => backend.backendId === candidate)
      ) {
        candidate = `${provider}-${suffix++}`;
      }
      return candidate;
    })();
  const existingProfile =
    deployment.inferenceProfiles.find((row) => row.backend_id === backendId) ??
    (!addingBackend && !addingExtra ? deployment.inferenceProfiles[0] : undefined);
  const profileId = existingProfile?.profile_id ?? `profile-${backendId}`;
  const samplingId =
    recommendation.temperature || recommendation.topP
      ? (existingProfile?.sampling_id ?? `${profileId}-sampling`)
      : null;
  const defaultBehaviorId = deployment.agentPrincipal.defaultBehaviorId;
  if (!defaultBehaviorId && !addingBackend) {
    throw new Error("The agent has no default behavior to activate for Setup");
  }
  const shouldRebindDefault = shouldRebindSetupDefault(deployment, addingExtra);
  const behaviorConfigs = (addingBackend ? [] : deployment.behaviorConfigs)
    .filter(
      (behavior) =>
        !behavior.inference_profile_id ||
        (behavior.behavior_id === defaultBehaviorId && shouldRebindDefault),
    )
    .map((behavior) => ({
      ...behavior,
      inference_profile_id: profileId,
    }));

  return {
    profileId,
    defaultBehaviorId,
    document: {
      agent_principal: { agent_did: deployment.agentDid },
      inference_backends: [
        {
          agent_did: deployment.agentDid,
          backend_id: backendId,
          name: discovery.backendName,
          provider_kind: discovery.providerKind,
          openai_wire_api: discovery.openaiWireApi,
          endpoint: discovery.effectiveEndpoint,
          auth: oauth
            ? { kind: "principal_oauth" }
            : apiKey.trim()
              ? { kind: "api_key", key: apiKey.trim() }
              : { kind: "unauthenticated" },
          max_concurrent: Number(settings.maxConcurrent),
          max_queue_depth: 100,
          enabled: true,
        },
      ],
      inference_profiles: [
        {
          ...existingProfile,
          agent_did: deployment.agentDid,
          profile_id: profileId,
          display_name: discovery.backendName,
          backend_id: backendId,
          model_name: model.trim(),
          reasoning_effort: settings.reasoningEffort || null,
          context_window: recommendation.contextWindow
            ? Number(settings.contextWindow)
            : null,
          max_output_tokens: recommendation.maxOutputTokens
            ? Number(settings.maxOutputTokens)
            : null,
          sampling_id: samplingId,
        },
      ],
      ...(samplingId
        ? {
            inference_sampling: [
              {
                agent_did: deployment.agentDid,
                sampling_id: samplingId,
                display_name: `${discovery.backendName} recommended sampling`,
                temperature: recommendation.temperature
                  ? Number(settings.temperature)
                  : null,
                top_p: recommendation.topP ? Number(settings.topP) : null,
              },
            ],
          }
        : {}),
      ...(behaviorConfigs.length ? { agent_behaviors: behaviorConfigs } : {}),
    },
  };
}
