import { spawn } from "node:child_process";

import {
  applyMockInference,
  resolveLivePlaywrightOptions,
} from "./live-playwright-options.mjs";
import { startMockInference } from "./mock-inference.mjs";

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
