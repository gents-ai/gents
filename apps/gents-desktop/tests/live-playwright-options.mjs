export const DEFAULT_MOCK_MODEL_NAME = "desktop-live-browser-mock";

export function takeFlag(argv, name) {
  const directPrefix = `${name}=`;
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === name) {
      const next = argv[index + 1];
      if (!next || next.startsWith("--")) {
        throw new Error(`missing value for ${name}`);
      }
      argv.splice(index, 2);
      return next;
    }
    if (value.startsWith(directPrefix)) {
      argv.splice(index, 1);
      return value.slice(directPrefix.length);
    }
  }
  return null;
}

export function takeSwitch(argv, name) {
  const index = argv.indexOf(name);
  if (index === -1) {
    return false;
  }
  argv.splice(index, 1);
  return true;
}

export function truthy(value) {
  return value === "1" || value === "true";
}

export function resolveLivePlaywrightOptions(rawArgv, rawEnv) {
  const argv = [...rawArgv];
  const requireRealInference = takeSwitch(argv, "--require-real-inference");
  const inferenceUrl = takeFlag(argv, "--inference-url");
  const modelName = takeFlag(argv, "--model-name");
  const provider = takeFlag(argv, "--provider");
  const apiKey = takeFlag(argv, "--api-key");
  const apiKeyEnvVar = takeFlag(argv, "--api-key-env-var");
  const agentTargetInferenceUrl = takeFlag(argv, "--agent-target-inference-url");
  const agentTargetModelName = takeFlag(argv, "--agent-target-model-name");
  const agentTargetProvider = takeFlag(argv, "--agent-target-provider");
  const agentTargetApiKey = takeFlag(argv, "--agent-target-api-key");
  const agentTargetApiKeyEnvVar = takeFlag(argv, "--agent-target-api-key-env-var");

  const env = {
    ...rawEnv,
    CARGO_NET_GIT_FETCH_WITH_CLI: rawEnv.CARGO_NET_GIT_FETCH_WITH_CLI ?? "true",
    GENTS_TAURI_LIVE: "1",
  };

  const configuredInferenceUrl =
    inferenceUrl ??
    env.GENTS_TAURI_LIVE_INFERENCE_URL ??
    env.GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT;
  const configuredModelName =
    modelName ??
    env.GENTS_TAURI_LIVE_MODEL_NAME ??
    env.GENTS_DESKTOP_LIVE_BACKEND_MODEL;
  const mustUseRealInference =
    requireRealInference || truthy(env.GENTS_TAURI_LIVE_REQUIRE_REAL_INFERENCE);

  if (mustUseRealInference && !configuredInferenceUrl && !env.OPENROUTER_API_KEY) {
    throw new Error(
      [
        "live Playwright real-inference mode requires a backend.",
        "Pass --inference-url <url> --model-name <model>,",
        "set GENTS_TAURI_LIVE_INFERENCE_URL/GENTS_TAURI_LIVE_MODEL_NAME,",
        "or set OPENROUTER_API_KEY for the OpenRouter fallback.",
      ].join(" "),
    );
  }

  const shouldStartMockInference =
    !mustUseRealInference && !configuredInferenceUrl && !env.OPENROUTER_API_KEY;
  if (!shouldStartMockInference) {
    applyResolvedInference(env, configuredInferenceUrl, configuredModelName);
  }

  applyProviderFlags(env, {
    provider,
    apiKey,
    apiKeyEnvVar,
    agentTargetInferenceUrl,
    agentTargetModelName,
    agentTargetProvider,
    agentTargetApiKey,
    agentTargetApiKeyEnvVar,
  });

  return {
    argv,
    env,
    shouldStartMockInference,
    mockModelName: configuredModelName ?? DEFAULT_MOCK_MODEL_NAME,
  };
}

export function applyMockInference(env, mockInference) {
  const next = { ...env };
  applyResolvedInference(next, mockInference.endpoint, mockInference.modelName);
  next.GENTS_TAURI_LIVE_PROVIDER ??= "OpenAiCompatible";
  next.GENTS_TAURI_LIVE_API_KEY ??= "desktop-live-browser-test-key";
  return next;
}

function applyResolvedInference(env, inferenceUrl, modelName) {
  if (inferenceUrl) {
    env.GENTS_TAURI_LIVE_INFERENCE_URL = inferenceUrl;
    env.GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT ??= inferenceUrl;
  }
  if (modelName) {
    env.GENTS_TAURI_LIVE_MODEL_NAME = modelName;
    env.GENTS_DESKTOP_LIVE_BACKEND_MODEL ??= modelName;
  }
}

function applyProviderFlags(
  env,
  {
    provider,
    apiKey,
    apiKeyEnvVar,
    agentTargetInferenceUrl,
    agentTargetModelName,
    agentTargetProvider,
    agentTargetApiKey,
    agentTargetApiKeyEnvVar,
  },
) {
  if (provider) env.GENTS_TAURI_LIVE_PROVIDER = provider;
  if (apiKey) env.GENTS_TAURI_LIVE_API_KEY = apiKey;
  if (apiKeyEnvVar) env.GENTS_TAURI_LIVE_API_KEY_ENV_VAR = apiKeyEnvVar;
  if (agentTargetInferenceUrl)
    env.GENTS_TAURI_LIVE_AGENT_TARGET_INFERENCE_URL = agentTargetInferenceUrl;
  if (agentTargetModelName)
    env.GENTS_TAURI_LIVE_AGENT_TARGET_MODEL_NAME = agentTargetModelName;
  if (agentTargetProvider)
    env.GENTS_TAURI_LIVE_AGENT_TARGET_PROVIDER = agentTargetProvider;
  if (agentTargetApiKey) env.GENTS_TAURI_LIVE_AGENT_TARGET_API_KEY = agentTargetApiKey;
  if (agentTargetApiKeyEnvVar)
    env.GENTS_TAURI_LIVE_AGENT_TARGET_API_KEY_ENV_VAR = agentTargetApiKeyEnvVar;
}
