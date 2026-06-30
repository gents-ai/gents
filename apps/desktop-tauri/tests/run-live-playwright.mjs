import { spawn } from "node:child_process";
import { createServer } from "node:http";

import {
  applyMockInference,
  resolveLivePlaywrightOptions,
} from "./live-playwright-options.mjs";

let mockInference = null;
const options = resolveLivePlaywrightOptions(process.argv.slice(2), process.env);
let env = options.env;

if (options.shouldStartMockInference) {
  mockInference = await startMockInference(options.mockModelName);
  env = applyMockInference(env, mockInference);
  console.error(
    `[live-playwright] using local mock inference endpoint ${mockInference.endpoint}`,
  );
}

const child = spawn(
  "npx",
  ["playwright", "test", "-c", "playwright.live.config.ts", ...options.argv],
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
