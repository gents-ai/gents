import { spawn } from "node:child_process";

function takeFlag(argv, name) {
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

const DEFAULT_LIVE_INFERENCE_URL = "http://100.69.4.79:8000/v1";
const DEFAULT_LIVE_MODEL_NAME = "baa-ai/GLM-5.1-RAM-420GB-MLX";

const argv = [...process.argv.slice(2)];
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
  ...process.env,
  CARGO_NET_GIT_FETCH_WITH_CLI: process.env.CARGO_NET_GIT_FETCH_WITH_CLI ?? "true",
  DEFRA_AGENT_TAURI_LIVE: "1",
};

const resolvedInferenceUrl =
  inferenceUrl ??
  env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL ??
  env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT ??
  DEFAULT_LIVE_INFERENCE_URL;
const resolvedModelName =
  modelName ??
  env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME ??
  env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL ??
  DEFAULT_LIVE_MODEL_NAME;

env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL = resolvedInferenceUrl;
env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME = resolvedModelName;
env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT ??= resolvedInferenceUrl;
env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL ??= resolvedModelName;

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

const child = spawn(
  "npx",
  ["playwright", "test", "-c", "playwright.live.config.ts", ...argv],
  {
    stdio: "inherit",
    env,
  },
);

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
