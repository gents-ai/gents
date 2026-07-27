#!/usr/bin/env node
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { chmod, mkdir, writeFile } from "node:fs/promises";
import { delimiter, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { runWithWatchdogRetry, stopProcess } from "./runner-control.mjs";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const args = process.argv.slice(2);
const headed = consumeFlag(args, "--headed");
const keepRunning = consumeFlag(args, "--no-exit-on-violation");
const port = await choosePort(process.env.BOMBADIL_VITE_PORT);
const origin = `http://127.0.0.1:${port}`;
const harnessUrl = `${origin}/tests/ui-harness/harness.html`;
const defaultOutputPath = process.env.BOMBADIL_OUTPUT_PATH
  ? resolve(process.env.BOMBADIL_OUTPUT_PATH)
  : resolve(rootDir, "test-results", "bombadil", String(Date.now()));
let bombadilProcess = null;

const vite = spawn(
  process.execPath,
  [
    // Vite may be hoisted to the workspace root; resolve its package root
    // through Node (its exports map hides bin/).
    resolve(
      dirname(createRequire(import.meta.url).resolve("vite/package.json")),
      "bin/vite.js",
    ),
    "--host",
    "127.0.0.1",
    "--port",
    String(port),
    "--strictPort",
    "--clearScreen",
    "false",
  ],
  {
    cwd: rootDir,
    stdio: ["ignore", "pipe", "pipe"],
  },
);

const viteOutput = [];
vite.stdout.on("data", (chunk) => bufferViteOutput(viteOutput, chunk));
vite.stderr.on("data", (chunk) => bufferViteOutput(viteOutput, chunk));
installSignalHandlers();

try {
  await waitForVite(vite, harnessUrl);

  const bombadilArgs = ["browser", "test", harnessUrl, "tests/bombadil/spec.ts"];
  if (!headed) {
    bombadilArgs.push("--headless");
  }
  if (!keepRunning) {
    bombadilArgs.push("--exit-on-violation");
  }
  if (!hasOption(args, "--time-limit") && !hasOption(args, "--reproduce")) {
    bombadilArgs.push("--time-limit", process.env.BOMBADIL_TIME_LIMIT ?? "20s");
  }
  if (!hasOption(args, "--output-path")) {
    bombadilArgs.push("--output-path", defaultOutputPath);
  }
  bombadilArgs.push(...args);

  const outputPath = outputPathFromArgs(bombadilArgs);
  const chromeEnv = await chromePathEnvironment(outputPath);
  const useProcessGroup = process.platform !== "win32";
  const runnerTimeoutMs = runnerTimeoutMillis(bombadilArgs);
  const result = await runWithWatchdogRetry({
    timeoutMs: runnerTimeoutMs,
    maxWatchdogRetries: 1,
    startAttempt: async (attempt) => {
      const attemptArgs = bombadilArgs.slice();
      if (attempt > 1) {
        setOutputPath(attemptArgs, `${outputPath}-watchdog-retry-${attempt - 1}`);
      }
      const attemptOutputPath = outputPathFromArgs(attemptArgs);
      await writeRunReadme(attemptOutputPath, harnessUrl, attemptArgs);

      console.log(`[bombadil] harness: ${harnessUrl}`);
      console.log(`[bombadil] output: ${attemptOutputPath}`);
      bombadilProcess = spawn(resolveBin("bombadil"), attemptArgs, {
        cwd: rootDir,
        detached: useProcessGroup,
        env: {
          ...process.env,
          ...chromeEnv,
          RUST_LOG: process.env.BOMBADIL_RUST_LOG ?? "error",
        },
        stdio: "inherit",
      });
      return bombadilProcess;
    },
    stopTimedOutChild: (child) =>
      stopProcess(child, { killProcessGroup: useProcessGroup }),
    onRetry: ({ nextAttempt }) => {
      console.warn(
        `[bombadil] runner watchdog expired; retrying once with a fresh browser process (attempt ${nextAttempt}/2)`,
      );
    },
  });
  if (result.kind === "timeout") {
    throw new Error(
      `Bombadil did not exit before the runner watchdog (${runnerTimeoutMs}ms) after ${result.attempts} attempts`,
    );
  }
  process.exitCode = result.code ?? 1;
} catch (error) {
  process.exitCode = 1;
  console.error(`[bombadil] ${error instanceof Error ? error.message : String(error)}`);
  if (viteOutput.length > 0) {
    console.error("[vite output]");
    console.error(viteOutput.join(""));
  }
} finally {
  await stopProcess(bombadilProcess, {
    killProcessGroup: process.platform !== "win32",
  });
  await stopProcess(vite);
}

async function resolveChromeExecutable() {
  const explicit =
    process.env.BOMBADIL_CHROME_EXECUTABLE ??
    process.env.CHROME ??
    process.env.CHROME_PATH ??
    process.env.CHROME_BIN;
  if (explicit) {
    if (!existsSync(explicit)) {
      throw new Error(`configured Chrome executable does not exist: ${explicit}`);
    }
    return explicit;
  }

  try {
    const { chromium } = await import("playwright");
    const executablePath = chromium.executablePath();
    if (existsSync(executablePath)) {
      return executablePath;
    }
  } catch {
    // Fall through to Bombadil's managed-browser path below.
  }

  if (process.env.CI) {
    throw new Error(
      "Playwright Chromium is not installed; run `npx playwright install chromium` before Bombadil.",
    );
  }
  return null;
}

async function chromePathEnvironment(outputPath) {
  const chromeExecutable = await resolveChromeExecutable();
  if (!chromeExecutable) {
    return {};
  }

  const binDir = resolve(outputPath, "chrome-bin");
  await mkdir(binDir, { recursive: true });
  for (const name of ["chromium-browser", "chromium", "google-chrome", "chrome"]) {
    const wrapper = resolve(binDir, name);
    await writeFile(wrapper, `#!/bin/sh\nexec ${shellQuote(chromeExecutable)} "$@"\n`);
    await chmod(wrapper, 0o755);
  }

  console.log(`[bombadil] chrome: ${chromeExecutable}`);
  return {
    PATH: `${binDir}${delimiter}${process.env.PATH ?? ""}`,
    CHROME: process.env.CHROME ?? chromeExecutable,
    CHROME_PATH: process.env.CHROME_PATH ?? chromeExecutable,
    CHROME_BIN: process.env.CHROME_BIN ?? chromeExecutable,
  };
}

function shellQuote(value) {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function resolveBin(name) {
  const suffix = process.platform === "win32" ? ".cmd" : "";
  return resolve(rootDir, "node_modules", ".bin", `${name}${suffix}`);
}

function consumeFlag(values, flag) {
  const index = values.indexOf(flag);
  if (index < 0) {
    return false;
  }
  values.splice(index, 1);
  return true;
}

function hasOption(values, option) {
  return values.some((value) => value === option || value.startsWith(`${option}=`));
}

function outputPathFromArgs(values) {
  const index = values.indexOf("--output-path");
  if (index >= 0 && values[index + 1]) {
    return values[index + 1];
  }
  const assigned = values.find((value) => value.startsWith("--output-path="));
  return assigned ? assigned.slice("--output-path=".length) : "(bombadil default)";
}

function setOutputPath(values, outputPath) {
  const index = values.indexOf("--output-path");
  if (index >= 0) {
    values[index + 1] = outputPath;
    return;
  }
  const assigned = values.findIndex((value) => value.startsWith("--output-path="));
  if (assigned >= 0) {
    values[assigned] = `--output-path=${outputPath}`;
    return;
  }
  values.push("--output-path", outputPath);
}

async function writeRunReadme(outputPath, harnessUrl, bombadilArgs) {
  if (outputPath === "(bombadil default)") {
    return;
  }
  const outputDir = resolve(rootDir, outputPath);
  await mkdir(outputDir, { recursive: true });
  const inspectCommand = `npx bombadil browser inspect ${shellQuote(outputDir)}`;
  const reproduceCommand = `npm run test:ui:fuzz -- --reproduce ${shellQuote(outputDir)}`;
  const directCommand = `npx bombadil ${bombadilArgs.map(shellQuote).join(" ")}`;
  await writeFile(
    resolve(outputDir, "README.md"),
    [
      "# Bombadil Desktop UI Run",
      "",
      `Created: ${new Date().toISOString()}`,
      `Harness: ${harnessUrl}`,
      "",
      "Inspect the run:",
      "",
      "```bash",
      inspectCommand,
      "```",
      "",
      "Reproduce through the desktop npm wrapper:",
      "",
      "```bash",
      reproduceCommand,
      "```",
      "",
      "Direct Bombadil command used by the wrapper:",
      "",
      "```bash",
      directCommand,
      "```",
      "",
      "Notes:",
      "",
      "- The npm wrapper starts Vite on a fresh local port before replaying.",
      "- A watchdog retry writes to the adjacent `-watchdog-retry-1` directory so both attempts remain inspectable.",
      "- Use this directory path in GitHub bug issues as the artifact reference.",
      "",
    ].join("\n"),
  );
}

function bufferViteOutput(buffer, chunk) {
  buffer.push(String(chunk));
  while (buffer.length > 20) {
    buffer.shift();
  }
}

async function choosePort(explicitPort) {
  const requested = explicitPort ? Number(explicitPort) : 0;
  if (!Number.isInteger(requested) || requested < 0 || requested > 65535) {
    throw new Error(`invalid BOMBADIL_VITE_PORT: ${explicitPort}`);
  }
  return new Promise((resolvePort, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen({ host: "127.0.0.1", port: requested }, () => {
      const address = server.address();
      server.close(() => {
        if (address && typeof address === "object") {
          resolvePort(address.port);
        } else {
          reject(new Error("could not allocate a Vite port"));
        }
      });
    });
  });
}

function runnerTimeoutMillis(values) {
  if (process.env.BOMBADIL_RUNNER_TIMEOUT_MS) {
    const value = Number(process.env.BOMBADIL_RUNNER_TIMEOUT_MS);
    if (Number.isFinite(value) && value > 0) {
      return value;
    }
    throw new Error(
      `invalid BOMBADIL_RUNNER_TIMEOUT_MS: ${process.env.BOMBADIL_RUNNER_TIMEOUT_MS}`,
    );
  }

  const timeLimitMs = timeLimitMillis(values);
  if (timeLimitMs === null) {
    return null;
  }
  return Math.max(timeLimitMs * 3, timeLimitMs + 30_000);
}

function timeLimitMillis(values) {
  const index = values.indexOf("--time-limit");
  if (index >= 0) {
    return parseDurationMillis(values[index + 1]);
  }
  const assigned = values.find((value) => value.startsWith("--time-limit="));
  if (assigned) {
    return parseDurationMillis(assigned.slice("--time-limit=".length));
  }
  return null;
}

function parseDurationMillis(value) {
  const match = /^(\d+(?:\.\d+)?)([smhd])$/.exec(value ?? "");
  if (!match) {
    throw new Error(`invalid --time-limit: ${value}`);
  }
  const amount = Number(match[1]);
  const unit = match[2];
  const scale =
    unit === "s"
      ? 1_000
      : unit === "m"
        ? 60_000
        : unit === "h"
          ? 3_600_000
          : 86_400_000;
  return amount * scale;
}

async function waitForVite(process, url) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (process.exitCode !== null) {
      throw new Error(`Vite exited early with status ${process.exitCode}`);
    }
    try {
      const response = await fetch(url, { method: "HEAD" });
      if (response.ok) {
        return;
      }
    } catch {
      // Server is not ready yet.
    }
    await delay(200);
  }
  throw new Error(`timed out waiting for Vite at ${url}`);
}

function delay(ms) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, ms));
}

function installSignalHandlers() {
  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.once(signal, () => {
      void (async () => {
        await stopProcess(bombadilProcess, {
          killProcessGroup: process.platform !== "win32",
        });
        await stopProcess(vite);
        process.exit(signal === "SIGINT" ? 130 : 143);
      })();
    });
  }
}
