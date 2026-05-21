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

const argv = [...process.argv.slice(2)];
const inferenceUrl = takeFlag(argv, "--inference-url");
const modelName = takeFlag(argv, "--model-name");
const provider = takeFlag(argv, "--provider");
const apiKey = takeFlag(argv, "--api-key");
const apiKeyEnvVar = takeFlag(argv, "--api-key-env-var");

const env = {
  ...process.env,
  CARGO_NET_GIT_FETCH_WITH_CLI:
    process.env.CARGO_NET_GIT_FETCH_WITH_CLI ?? "true",
  DEFRA_AGENT_TAURI_LIVE: "1",
};

if (inferenceUrl) {
  env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL = inferenceUrl;
}
if (modelName) {
  env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME = modelName;
}
if (provider) {
  env.DEFRA_AGENT_TAURI_LIVE_PROVIDER = provider;
}
if (apiKey) {
  env.DEFRA_AGENT_TAURI_LIVE_API_KEY = apiKey;
}
if (apiKeyEnvVar) {
  env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR = apiKeyEnvVar;
}

const liveTestFiles = [
  "tests/tauri-driver.live.behavior.test.tsx",
  "tests/tauri-driver.live.chat.test.tsx",
  "tests/tauri-driver.live.config.test.tsx",
  "tests/tauri-driver.live.interrupt.test.tsx",
  "tests/tauri-driver.live.sad-path.test.tsx",
];

const child = spawn(
  "npx",
  ["vitest", "run", ...liveTestFiles, ...argv],
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
