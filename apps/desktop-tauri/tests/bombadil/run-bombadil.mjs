#!/usr/bin/env node
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createServer } from "node:net";
import { chmod, mkdir, writeFile } from "node:fs/promises";
import { delimiter, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
    resolve(rootDir, "node_modules/vite/bin/vite.js"),
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
  await mkdir(defaultOutputPath, { recursive: true });

  const chromeEnv = await chromePathEnvironment(defaultOutputPath);
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

  console.log(`[bombadil] harness: ${harnessUrl}`);
  console.log(`[bombadil] output: ${outputPathFromArgs(bombadilArgs)}`);

  bombadilProcess = spawn(resolveBin("bombadil"), bombadilArgs, {
    cwd: rootDir,
    env: {
      ...process.env,
      ...chromeEnv,
      RUST_LOG: process.env.BOMBADIL_RUST_LOG ?? "error",
    },
    stdio: "inherit",
  });
  const runnerTimeoutMs = runnerTimeoutMillis(bombadilArgs);
  const result = await waitForExitWithTimeout(bombadilProcess, runnerTimeoutMs);
  if (result.kind === "timeout") {
    await stopProcess(bombadilProcess);
    throw new Error(
      `Bombadil did not exit before the runner watchdog (${runnerTimeoutMs}ms)`,
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

function waitForExitWithTimeout(process, timeoutMs) {
  if (timeoutMs === null) {
    return waitForExit(process).then((code) => ({ kind: "exit", code }));
  }
  return new Promise((resolveExit) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) {
        return;
      }
      settled = true;
      resolveExit({ kind: "timeout" });
    }, timeoutMs);
    process.once("exit", (code) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      resolveExit({ kind: "exit", code });
    });
  });
}

function waitForExit(process) {
  return new Promise((resolveExit) => {
    process.once("exit", (code) => resolveExit(code));
  });
}

async function stopProcess(process) {
  if (!process) {
    return;
  }
  if (process.exitCode !== null || process.signalCode !== null) {
    return;
  }
  process.kill("SIGTERM");
  const code = await Promise.race([
    waitForExit(process),
    delay(2_000).then(() => null),
  ]);
  if (code === null && process.exitCode === null) {
    process.kill("SIGKILL");
  }
}

function delay(ms) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, ms));
}

function installSignalHandlers() {
  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.once(signal, () => {
      void (async () => {
        await stopProcess(bombadilProcess);
        await stopProcess(vite);
        process.exit(signal === "SIGINT" ? 130 : 143);
      })();
    });
  }
}
