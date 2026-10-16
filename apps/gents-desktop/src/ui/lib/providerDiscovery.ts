import type { InferenceDiscoveryResult } from "@source-inc/gents-desktop-client";

export function inferenceDiscoveryKey(
  revision: number,
  provider: string,
  authMethod: string,
  endpoint: string,
) {
  return `${revision}:${provider}:${authMethod}:${endpoint.trim()}`;
}

export function currentInferenceDiscovery(
  currentKey: string,
  result: InferenceDiscoveryResult,
) {
  return result.requestKey === currentKey ? result : null;
}
