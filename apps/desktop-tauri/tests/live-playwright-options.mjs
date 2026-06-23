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
  const subagentInferenceUrl = takeFlag(argv, "--subagent-inference-url");
  const subagentModelName = takeFlag(argv, "--subagent-model-name");
  const subagentProvider = takeFlag(argv, "--subagent-provider");
  const subagentApiKey = takeFlag(argv, "--subagent-api-key");
  const subagentApiKeyEnvVar = takeFlag(argv, "--subagent-api-key-env-var");

  const env = {
    ...rawEnv,
    CARGO_NET_GIT_FETCH_WITH_CLI: rawEnv.CARGO_NET_GIT_FETCH_WITH_CLI ?? "true",
    DEFRA_AGENT_TAURI_LIVE: "1",
  };

  const configuredInferenceUrl =
    inferenceUrl ??
    env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL ??
    env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT;
  const configuredModelName =
    modelName ??
    env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME ??
    env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL;
  const mustUseRealInference =
    requireRealInference || truthy(env.DEFRA_AGENT_TAURI_LIVE_REQUIRE_REAL_INFERENCE);

  if (mustUseRealInference && !configuredInferenceUrl && !env.OPENROUTER_API_KEY) {
    throw new Error(
      [
        "live Playwright real-inference mode requires a backend.",
        "Pass --inference-url <url> --model-name <model>,",
        "set DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL/DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,",
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
    subagentInferenceUrl,
    subagentModelName,
    subagentProvider,
    subagentApiKey,
    subagentApiKeyEnvVar,
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
  next.DEFRA_AGENT_TAURI_LIVE_PROVIDER ??= "openai-compatible";
  next.DEFRA_AGENT_TAURI_LIVE_API_KEY ??= "desktop-live-browser-test-key";
  next.DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS ??= "1";
  return next;
}

function applyResolvedInference(env, inferenceUrl, modelName) {
  if (inferenceUrl) {
    env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL = inferenceUrl;
    env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT ??= inferenceUrl;
  }
  if (modelName) {
    env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME = modelName;
    env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL ??= modelName;
  }
}

function applyProviderFlags(
  env,
  {
    provider,
    apiKey,
    apiKeyEnvVar,
    subagentInferenceUrl,
    subagentModelName,
    subagentProvider,
    subagentApiKey,
    subagentApiKeyEnvVar,
  },
) {
  if (provider) env.DEFRA_AGENT_TAURI_LIVE_PROVIDER = provider;
  if (apiKey) env.DEFRA_AGENT_TAURI_LIVE_API_KEY = apiKey;
  if (apiKeyEnvVar) env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR = apiKeyEnvVar;
  if (subagentInferenceUrl)
    env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_INFERENCE_URL = subagentInferenceUrl;
  if (subagentModelName)
    env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_MODEL_NAME = subagentModelName;
  if (subagentProvider) env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_PROVIDER = subagentProvider;
  if (subagentApiKey) env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_API_KEY = subagentApiKey;
  if (subagentApiKeyEnvVar)
    env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_API_KEY_ENV_VAR = subagentApiKeyEnvVar;
}
