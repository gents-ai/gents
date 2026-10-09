import { expect } from "vitest";

import type {
  ChatSendResult,
  DeploymentView,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { LiveBridgeRunner, type LiveBridgeRunnerOptions } from "../live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "../tauri-driver";

export const DEFAULT_LIVE_INFERENCE_URL = "http://100.73.235.38:8000/v1";
export const DEFAULT_LIVE_MODEL_NAME = "GLM-5.2";

export type LiveDesktopDriver = ReturnType<typeof renderTauriAppDriverWithBridge>;

export type LiveDesktopContext = {
  runner: LiveBridgeRunner;
  driver: LiveDesktopDriver;
  deployment: DeploymentView;
};

export function liveRunnerOptionsFromEnv(
  overrides: LiveBridgeRunnerOptions = {},
): LiveBridgeRunnerOptions {
  return {
    inferenceUrl:
      process.env.GENTS_TAURI_LIVE_INFERENCE_URL ??
      process.env.GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT ??
      DEFAULT_LIVE_INFERENCE_URL,
    modelName:
      process.env.GENTS_TAURI_LIVE_MODEL_NAME ??
      process.env.GENTS_DESKTOP_LIVE_BACKEND_MODEL ??
      DEFAULT_LIVE_MODEL_NAME,
    provider:
      process.env.GENTS_TAURI_LIVE_PROVIDER ??
      process.env.GENTS_DESKTOP_LIVE_BACKEND_PROVIDER,
    apiKey:
      process.env.GENTS_TAURI_LIVE_API_KEY ??
      process.env.GENTS_DESKTOP_LIVE_BACKEND_API_KEY,
    apiKeyEnvVar:
      process.env.GENTS_TAURI_LIVE_API_KEY_ENV_VAR ??
      process.env.GENTS_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR,
    agentTargetInferenceUrl:
      process.env.GENTS_TAURI_LIVE_AGENT_TARGET_INFERENCE_URL ??
      process.env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_ENDPOINT,
    agentTargetModelName:
      process.env.GENTS_TAURI_LIVE_AGENT_TARGET_MODEL_NAME ??
      process.env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_MODEL,
    agentTargetProvider:
      process.env.GENTS_TAURI_LIVE_AGENT_TARGET_PROVIDER ??
      process.env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_PROVIDER,
    agentTargetApiKey:
      process.env.GENTS_TAURI_LIVE_AGENT_TARGET_API_KEY ??
      process.env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_API_KEY,
    agentTargetApiKeyEnvVar:
      process.env.GENTS_TAURI_LIVE_AGENT_TARGET_API_KEY_ENV_VAR ??
      process.env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_API_KEY_ENV_VAR,
    ...overrides,
  };
}

export function expectFirstDeployment(snapshot: DesktopClientSnapshot) {
  const deployment = snapshot.client?.deployments[0];
  expect(deployment, "live desktop runner did not expose a deployment").toBeDefined();
  return deployment!;
}

export async function startLiveDesktop(
  options: LiveBridgeRunnerOptions = {},
): Promise<LiveDesktopContext> {
  const runner = await LiveBridgeRunner.start(liveRunnerOptionsFromEnv(options));
  try {
    const initialSnapshot = await runner.fetchSnapshot();
    const deployment = expectFirstDeployment(initialSnapshot);
    const driver = renderTauriAppDriverWithBridge(runner, deployment.peerId);
    return { runner, driver, deployment };
  } catch (error) {
    await runner.dispose();
    throw error;
  }
}

export async function withLiveDesktop<T>(
  callback: (context: LiveDesktopContext) => Promise<T>,
  options: LiveBridgeRunnerOptions = {},
) {
  const context = await startLiveDesktop(options);
  try {
    return await callback(context);
  } finally {
    await context.driver.dispose();
  }
}

export function expectLatestSendResult(
  runner: LiveBridgeRunner,
  label = "chat request",
): ChatSendResult {
  const result = runner.sendResults.at(-1);
  expect(result, `${label} was not submitted`).toBeDefined();
  return result!;
}
