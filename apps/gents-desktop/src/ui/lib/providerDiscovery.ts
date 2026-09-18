import type { InferenceDiscoveryResult } from "@source-inc/gents-desktop-client";

export function inferenceDiscoveryKey(
  revision: number,
  provider: string,
  authMethod: string,
  endpoint: string,
) {
  return `${revision}:${provider}:${authMethod}:${endpoint.trim()}`;
}

/** A late Tauri response may only update the connection that launched it. */
export function currentInferenceDiscovery(
  currentKey: string,
  result: InferenceDiscoveryResult,
) {
  return result.requestKey === currentKey ? result : null;
}
