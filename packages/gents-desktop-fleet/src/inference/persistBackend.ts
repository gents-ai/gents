import type {
  BackendAuth,
  BackendProviderKind,
  ConfigComponentsPatchRequest,
  DeploymentView,
  OpenAiWireApi,
} from "@source-inc/gents-desktop-client";

import { resolveTargets } from "./resolveTargets.js";

export type PersistBackendOptions = {
  name: string;
  providerKind: BackendProviderKind;
  endpoint: string;
  modelName: string;
  auth: BackendAuth;
  openaiWireApi?: OpenAiWireApi;
};

export async function persistInferenceBackend({
  deployment,
  options,
  onPatchConfigComponents,
}: {
  deployment: DeploymentView;
  options: PersistBackendOptions;
  onPatchConfigComponents: (
    request: ConfigComponentsPatchRequest,
  ) => Promise<unknown>;
}) {
  const targets = resolveTargets(deployment);
  if (!targets.profile || !targets.backend) {
    throw new Error(targets.error ?? "Inference target binding is unavailable");
  }
  await onPatchConfigComponents({
    agentDid: deployment.agentDid,
    patches: [
      {
        collection: "InferenceBackend",
        id: targets.backend.backendId,
        changes: {
          name: options.name,
          provider_kind: options.providerKind,
          endpoint: options.endpoint,
          auth: options.auth,
          openai_wire_api: options.openaiWireApi ?? null,
          enabled: true,
        },
      },
      {
        collection: "InferenceProfile",
        id: targets.profile.profile_id,
        changes: { model_name: options.modelName },
      },
    ],
  });
}
