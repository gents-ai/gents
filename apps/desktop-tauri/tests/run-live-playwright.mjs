import { spawn } from "node:child_process";
import { createServer } from "node:http";

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

const DEFAULT_MOCK_MODEL_NAME = "desktop-live-browser-mock";

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

let mockInference = null;
const configuredInferenceUrl =
  inferenceUrl ??
  env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL ??
  env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT;
const configuredModelName =
  modelName ??
  env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME ??
  env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL;
let resolvedInferenceUrl = configuredInferenceUrl;
let resolvedModelName = configuredModelName;

if (!resolvedInferenceUrl && !env.OPENROUTER_API_KEY) {
  mockInference = await startMockInference(
    resolvedModelName ?? DEFAULT_MOCK_MODEL_NAME,
  );
  resolvedInferenceUrl = mockInference.endpoint;
  resolvedModelName = mockInference.modelName;
  env.DEFRA_AGENT_TAURI_LIVE_PROVIDER ??= "openai-compatible";
  env.DEFRA_AGENT_TAURI_LIVE_API_KEY ??= "desktop-live-browser-test-key";
  env.DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS ??= "1";
  console.error(
    `[live-playwright] using local mock inference endpoint ${mockInference.endpoint}`,
  );
}

if (resolvedInferenceUrl) {
  env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL = resolvedInferenceUrl;
  env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT ??= resolvedInferenceUrl;
}
if (resolvedModelName) {
  env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME = resolvedModelName;
  env.DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL ??= resolvedModelName;
}

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
  mockInference?.server.close();
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});

function startMockInference(modelName) {
  const finalText = "Desktop live browser smoke confirmation.";
  return new Promise((resolve, reject) => {
    const server = createServer((request, response) => {
      const path = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
      if (request.method === "GET" && (path === "/v1/models" || path === "/models")) {
        writeJson(response, 200, { data: [{ id: modelName }] });
        return;
      }
      if (
        request.method === "POST" &&
        (path === "/v1/chat/completions" || path === "/chat/completions")
      ) {
        drainRequest(request, () => {
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(completionTextSse(finalText));
        });
        return;
      }
      writeJson(response, 404, { error: "not found" });
    });
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("mock inference endpoint did not bind a TCP port"));
        return;
      }
      resolve({
        endpoint: `http://127.0.0.1:${address.port}/v1`,
        modelName,
        server,
      });
    });
  });
}

function drainRequest(request, done) {
  request.resume();
  request.on("end", done);
}

function writeJson(response, status, body) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(body));
}

function completionTextSse(text) {
  const chunk = {
    choices: [{ delta: { content: text }, finish_reason: null }],
    usage: null,
  };
  const finish = {
    choices: [{ delta: { content: null, tool_calls: [] }, finish_reason: "stop" }],
    usage: { prompt_tokens: 24, completion_tokens: 6, total_tokens: 30 },
  };
  return `data: ${JSON.stringify(chunk)}\n\ndata: ${JSON.stringify(finish)}\n\ndata: [DONE]\n\n`;
}
