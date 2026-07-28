import type {
  BackendSaveRequest,
  BehaviorSaveRequest,
  DeploymentView,
} from "@source-inc/gents-desktop-client";

import { behaviorSaveFrom, resolveTargets } from "./resolveTargets.js";

export type PersistBackendOptions = {
  name: string;
  providerKind: string;
  endpoint: string;
  models: string[];
  apiKey?: string;
  clearApiKey?: boolean;
  openaiWireApi?: string;
};

export async function persistInferenceBackend({
  deployment,
  options,
  onSaveBackendConfig,
  onSaveBehaviorConfig,
}: {
  deployment: DeploymentView;
  options: PersistBackendOptions;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
}) {
  const targets = resolveTargets(deployment);
  await onSaveBackendConfig({
    backendId: targets.backendId,
    name: options.name,
    providerKind: options.providerKind,
    openaiWireApi: options.openaiWireApi,
    endpoint: options.endpoint,
    apiKey: options.apiKey,
    maxConcurrent: targets.backend?.maxConcurrent ?? undefined,
    maxQueueDepth: targets.backend?.maxQueueDepth ?? undefined,
    clearApiKey: options.clearApiKey ?? false,
    models: options.models,
    enabled: true,
  });
  if (targets.behavior) {
    await onSaveBehaviorConfig(
      behaviorSaveFrom(
        targets.behavior,
        deployment.agentDid,
        targets.backendId,
      ),
    );
  }
}
