import { spawn } from "node:child_process";
import { mkdir } from "node:fs/promises";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { dirname, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright";

import { startMockInference } from "./mock-inference.mjs";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const APP_DIR = resolve(SCRIPT_DIR, "..");
const REPO_ROOT = resolve(APP_DIR, "../..");
const DEFAULT_MODEL = "gents-agent-browser-mock";
const DEFAULT_MOCK_RESPONSE = "Fleet E2E live agent-browser confirmation.";
const START_TIMEOUT_MS = 900_000;
const COMMAND_TIMEOUT_MS = 10_000;
const MAX_WAIT_MS = 60_000;
const MAX_SNAPSHOT_CHARS = 30_000;
const MAX_BODY_TEXT_CHARS = 12_000;
const INTERACTIVE_SELECTOR = [
  "button",
  "a[href]",
  "input",
  "textarea",
  "select",
  "summary",
  "[role]",
  "[tabindex]:not([tabindex='-1'])",
].join(", ");

const options = parseOptions(process.argv.slice(2), process.env);
if (options.help) {
  process.stdout.write(`${helpText()}\n`);
  process.exit(0);
}

const resources = {
  browser: null,
  context: null,
  live: null,
  mockInference: null,
  page: null,
  vite: null,
};
let cleanupPromise = null;
let shuttingDown = false;

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    if (shuttingDown) {
      return;
    }
    shuttingDown = true;
    process.stderr.write(`[agent-browser] received ${signal}; shutting down\n`);
    void cleanup().finally(() => process.exit(signal === "SIGINT" ? 130 : 143));
  });
}

try {
  const viewport = parseViewport(options.viewport);
  const port = await choosePort(options.port);
  const baseUrl = `http://127.0.0.1:${port}`;

  if (options.backend === "live" && !options.inferenceUrl) {
    resources.mockInference = await startMockInference(
      options.modelName,
      options.mockResponse,
    );
    options.inferenceUrl = resources.mockInference.endpoint;
    process.stderr.write(
      `[agent-browser] local mock inference: ${resources.mockInference.endpoint}\n`,
    );
  }

  resources.vite = spawnVite({ baseUrl, port });
  await resources.vite.startup;
  process.stderr.write(`[agent-browser] Vite: ${baseUrl}\n`);
  if (options.backend === "live") {
    resources.live = spawnLiveBridge(options);
    const { ready, logs } = await resources.live.startup;
    Object.assign(resources.live, { ready, logs });
    process.stderr.write(
      `[agent-browser] live bridge: ${ready.baseUrl} (${ready.deploymentLabel})\n`,
    );
  }

  const harnessUrl = buildHarnessUrl(baseUrl, options, resources.live?.ready.baseUrl);
  resources.browser = await chromium.launch({ headless: !options.headed });
  resources.context = await resources.browser.newContext(
    browserContextOptions(viewport, options.viewport),
  );
  resources.page = await resources.context.newPage();

  const browserEvents = [];
  resources.page.on("console", (message) => {
    browserEvents.push({
      kind: "console",
      level: message.type(),
      text: truncate(message.text(), 4_000),
    });
    trimEvents(browserEvents);
  });
  resources.page.on("pageerror", (error) => {
    browserEvents.push({
      kind: "pageerror",
      level: "error",
      text: truncate(error.stack ?? error.message, 4_000),
    });
    trimEvents(browserEvents);
  });

  await resources.page.goto(harnessUrl, {
    waitUntil: "domcontentloaded",
    timeout: 30_000,
  });
  await resources.page.locator(".app-shell").waitFor({
    state: "visible",
    timeout: options.backend === "live" ? 60_000 : 15_000,
  });
  await mkdir(options.artifactDir, { recursive: true });

  writeJsonLine({
    kind: "ready",
    protocolVersion: 1,
    backend: options.backend,
    scenario: options.backend === "deterministic" ? options.scenario : "live",
    url: harnessUrl,
    viewport,
    artifactDir: options.artifactDir,
    live: resources.live
      ? {
          agentDid: resources.live.ready.agentDid,
          deploymentLabel: resources.live.ready.deploymentLabel,
          toolRoot: resources.live.ready.toolRoot,
          inference: resources.mockInference ? "local-mock" : "configured",
        }
      : null,
    commands: [
      "snapshot",
      "inspect",
      "click",
      "fill",
      "press",
      "select",
      "check",
      "uncheck",
      "wait",
      "text",
      "screenshot",
      "console",
      "reload",
      "back",
      "goto",
      "viewport",
      "close",
    ],
  });

  const input = createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
    terminal: false,
  });
  let sequence = 0;
  let closeRequested = false;

  for await (const line of input) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    sequence += 1;
    let request;
    try {
      request = JSON.parse(trimmed);
    } catch (error) {
      writeJsonLine({
        id: null,
        ok: false,
        error: `invalid JSON: ${error.message}`,
      });
      continue;
    }

    const id = request.id ?? sequence;
    try {
      const result = await runCommand({
        request,
        page: resources.page,
        browserEvents,
        baseUrl,
        options,
      });
      writeJsonLine({ id, ok: true, result });
      if (request.command === "close") {
        closeRequested = true;
        break;
      }
    } catch (error) {
      const screenshot = await captureFailure(resources.page, options.artifactDir, id);
      writeJsonLine({
        id,
        ok: false,
        error: error.stack ?? error.message,
        screenshot,
        url: resources.page.url(),
        browserErrors: browserEvents.filter(
          (event) => event.kind === "pageerror" || event.level === "error",
        ),
      });
    }
  }

  input.close();
  process.stdin.pause();
  if (!closeRequested) {
    process.stderr.write("[agent-browser] input closed; shutting down\n");
  }
  await cleanup();
} catch (error) {
  writeJsonLine({
    kind: "fatal",
    ok: false,
    error: error.stack ?? error.message,
  });
  process.stdin.pause();
  await cleanup();
  process.exitCode = 1;
}

async function runCommand({ request, page, browserEvents, baseUrl, options }) {
  const command = requireString(request.command, "command");
  const timeout = clampTimeout(request.timeoutMs);

  switch (command) {
    case "snapshot": {
      const [title, bodyText, aria] = await Promise.all([
        page.title(),
        page.locator("body").innerText({ timeout }),
        page.locator("body").ariaSnapshot({ timeout }),
      ]);
      return {
        url: page.url(),
        title,
        viewport: page.viewportSize(),
        bodyText: truncate(normalizeText(bodyText), MAX_BODY_TEXT_CHARS),
        aria: truncate(aria, MAX_SNAPSHOT_CHARS),
        browserErrors: browserEvents.filter(
          (event) => event.kind === "pageerror" || event.level === "error",
        ),
      };
    }
    case "inspect": {
      const limit = clampNumber(request.limit, 1, 200, 100);
      const elements = await page.locator(INTERACTIVE_SELECTOR).evaluateAll(
        (nodes, maxElements) =>
          nodes
            .filter((node) => {
              const style = window.getComputedStyle(node);
              const rect = node.getBoundingClientRect();
              return (
                style.visibility !== "hidden" &&
                style.display !== "none" &&
                rect.width > 0 &&
                rect.height > 0
              );
            })
            .slice(0, maxElements)
            .map((node, index) => {
              const tag = node.tagName.toLowerCase();
              const labelledBy = (node.getAttribute("aria-labelledby") ?? "")
                .split(/\s+/)
                .filter(Boolean)
                .map((id) => document.getElementById(id)?.textContent ?? "")
                .join(" ");
              const labels =
                "labels" in node && node.labels
                  ? Array.from(node.labels)
                      .map((label) => label.textContent ?? "")
                      .join(" ")
                  : "";
              const role =
                node.getAttribute("role") ??
                (tag === "a"
                  ? "link"
                  : tag === "button" || tag === "summary"
                    ? "button"
                    : tag === "select"
                      ? "combobox"
                      : tag === "textarea"
                        ? "textbox"
                        : tag === "input"
                          ? node.getAttribute("type") === "checkbox"
                            ? "checkbox"
                            : node.getAttribute("type") === "radio"
                              ? "radio"
                              : "textbox"
                          : null);
              const name = [
                node.getAttribute("aria-label"),
                labelledBy,
                labels,
                node.getAttribute("alt"),
                node.getAttribute("title"),
                node.getAttribute("placeholder"),
                node.textContent,
              ]
                .find((value) => value?.trim())
                ?.replace(/\s+/g, " ")
                .trim();
              const inputType = node.getAttribute("type");
              const rawValue = "value" in node ? String(node.value ?? "") : null;
              return {
                index,
                tag,
                role,
                name: name ?? "",
                testId: node.getAttribute("data-testid"),
                type: inputType,
                disabled: "disabled" in node ? Boolean(node.disabled) : false,
                checked: "checked" in node ? Boolean(node.checked) : null,
                value: inputType === "password" && rawValue ? "<redacted>" : rawValue,
              };
            }),
        limit,
      );
      return { url: page.url(), count: elements.length, elements };
    }
    case "click": {
      const locator = resolveLocator(page, request.target);
      await locator.click({ timeout });
      return pageState(page);
    }
    case "fill": {
      const locator = resolveLocator(page, request.target);
      await locator.fill(requireString(request.value, "value"), { timeout });
      return pageState(page);
    }
    case "press": {
      const locator = resolveLocator(page, request.target);
      await locator.press(requireString(request.key, "key"), { timeout });
      return pageState(page);
    }
    case "select": {
      const locator = resolveLocator(page, request.target);
      const values = Array.isArray(request.value)
        ? request.value.map((value) => String(value))
        : requireString(request.value, "value");
      const selected = await locator.selectOption(values, { timeout });
      return { ...(await pageState(page)), selected };
    }
    case "check": {
      await resolveLocator(page, request.target).check({ timeout });
      return pageState(page);
    }
    case "uncheck": {
      await resolveLocator(page, request.target).uncheck({ timeout });
      return pageState(page);
    }
    case "wait": {
      if (request.target) {
        const state = request.state ?? "visible";
        if (!["attached", "detached", "visible", "hidden"].includes(state)) {
          throw new Error(`unsupported wait state: ${state}`);
        }
        await resolveLocator(page, request.target).waitFor({ state, timeout });
      } else {
        const milliseconds = clampNumber(request.ms, 0, MAX_WAIT_MS, 250);
        await page.waitForTimeout(milliseconds);
      }
      return pageState(page);
    }
    case "text": {
      const locator = resolveLocator(page, request.target);
      return {
        text: truncate(normalizeText(await locator.innerText({ timeout })), 20_000),
      };
    }
    case "screenshot": {
      return {
        path: await captureScreenshot(
          page,
          options.artifactDir,
          request.name ?? "agent-browser",
          Boolean(request.fullPage),
        ),
      };
    }
    case "console": {
      return { events: browserEvents };
    }
    case "reload": {
      await page.reload({ waitUntil: "domcontentloaded", timeout });
      await page.locator(".app-shell").waitFor({ state: "visible", timeout });
      return pageState(page);
    }
    case "back": {
      await page.goBack({ waitUntil: "domcontentloaded", timeout });
      return pageState(page);
    }
    case "goto": {
      if (options.backend !== "deterministic") {
        throw new Error("goto scenario is available only in deterministic mode");
      }
      const scenario = requireString(request.scenario, "scenario");
      const url = new URL("/tests/ui-harness/harness.html", baseUrl);
      url.searchParams.set("scenario", scenario);
      await page.goto(url.toString(), { waitUntil: "domcontentloaded", timeout });
      await page.locator(".app-shell").waitFor({ state: "visible", timeout });
      return pageState(page);
    }
    case "viewport": {
      const viewport = parseViewport(requireString(request.value, "value"));
      await page.setViewportSize(viewport);
      return pageState(page);
    }
    case "close":
      return { closing: true };
    default:
      throw new Error(`unsupported command: ${command}`);
  }
}

function resolveLocator(page, rawTarget) {
  if (!rawTarget || typeof rawTarget !== "object" || Array.isArray(rawTarget)) {
    throw new Error("target must be an object");
  }

  const strategies = ["testId", "role", "label", "placeholder", "text", "css"].filter(
    (key) => rawTarget[key] !== undefined,
  );
  if (strategies.length !== 1) {
    throw new Error(
      "target must contain exactly one of testId, role, label, placeholder, text, or css",
    );
  }

  const exact = rawTarget.exact === undefined ? true : Boolean(rawTarget.exact);
  let locator;
  if (rawTarget.testId !== undefined) {
    locator = page.getByTestId(String(rawTarget.testId));
  } else if (rawTarget.role !== undefined) {
    const roleOptions = {};
    if (rawTarget.name !== undefined) {
      roleOptions.name = String(rawTarget.name);
      roleOptions.exact = exact;
    }
    locator = page.getByRole(String(rawTarget.role), roleOptions);
  } else if (rawTarget.label !== undefined) {
    locator = page.getByLabel(String(rawTarget.label), { exact });
  } else if (rawTarget.placeholder !== undefined) {
    locator = page.getByPlaceholder(String(rawTarget.placeholder), { exact });
  } else if (rawTarget.text !== undefined) {
    locator = page.getByText(String(rawTarget.text), { exact });
  } else {
    locator = page.locator(String(rawTarget.css));
  }

  if (rawTarget.index !== undefined) {
    const index = clampNumber(rawTarget.index, 0, 10_000, 0);
    locator = locator.nth(index);
  }
  return locator;
}

async function pageState(page) {
  return {
    url: page.url(),
    title: await page.title(),
    viewport: page.viewportSize(),
  };
}

function spawnVite({ baseUrl, port }) {
  // Vite may be hoisted to the workspace root; resolve its package root
  // through Node (its exports map hides bin/) instead of assuming a nested
  // node_modules layout.
  const vitePath = resolve(
    dirname(createRequire(import.meta.url).resolve("vite/package.json")),
    "bin/vite.js",
  );
  const child = spawn(
    process.execPath,
    [
      vitePath,
      "--host",
      "127.0.0.1",
      "--port",
      String(port),
      "--strictPort",
      "--clearScreen",
      "false",
    ],
    {
      cwd: APP_DIR,
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  const logs = collectChildLogs(child);
  return {
    child,
    logs,
    // Return the child before readiness so fatal startup cleanup can always
    // stop it, including a live Vite process that never serves the harness.
    startup: waitForHttp(
      `${baseUrl}/tests/ui-harness/harness.html`,
      child,
      logs,
      30_000,
    ),
  };
}

function spawnLiveBridge(options) {
  const runnerArgs = [
    "run",
    "-p",
    "gents-desktop-tauri",
    "--bin",
    "bridge_runner",
    "--quiet",
    "--",
    "--inference-url",
    options.inferenceUrl,
    "--model-name",
    options.modelName,
    "--provider",
    options.provider,
  ];
  if (options.apiKeyEnvVar) {
    runnerArgs.push("--api-key-env-var", options.apiKeyEnvVar);
  } else {
    runnerArgs.push("--api-key", "gents-agent-browser-test-key");
  }

  const child = spawn("cargo", runnerArgs, {
    cwd: REPO_ROOT,
    detached: process.platform !== "win32",
    env: {
      ...process.env,
      CARGO_NET_GIT_FETCH_WITH_CLI: process.env.CARGO_NET_GIT_FETCH_WITH_CLI ?? "true",
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  return {
    child,
    logs: null,
    ready: null,
    startup: waitForRunnerReady(child, START_TIMEOUT_MS),
  };
}

function waitForRunnerReady(child, timeoutMs) {
  const logs = { stdout: [], stderr: [] };
  let stdoutBuffer = "";
  let settled = false;

  return new Promise((resolveReady, reject) => {
    const timer = setTimeout(() => {
      finish(
        reject,
        new Error(
          `bridge runner did not become ready within ${timeoutMs}ms\n${formatLogs(logs)}`,
        ),
      );
    }, timeoutMs);

    const finish = (callback, value) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      child.off("exit", onExit);
      callback(value);
    };

    const onExit = (code, signal) => {
      finish(
        reject,
        new Error(
          `bridge runner exited before ready (code=${code}, signal=${signal})\n${formatLogs(logs)}`,
        ),
      );
    };

    child.stdout.on("data", (chunk) => {
      stdoutBuffer += chunk.toString();
      let newline = stdoutBuffer.indexOf("\n");
      while (newline !== -1) {
        const line = stdoutBuffer.slice(0, newline).trim();
        stdoutBuffer = stdoutBuffer.slice(newline + 1);
        pushLog(logs.stdout, line);
        if (line) {
          try {
            const message = JSON.parse(line);
            if (message.kind === "ready") {
              finish(resolveReady, { ready: message, logs });
            }
          } catch {
            // Rust tracing can emit non-JSON output before the ready record.
          }
        }
        newline = stdoutBuffer.indexOf("\n");
      }
    });
    child.stderr.on("data", (chunk) => {
      for (const line of chunk.toString().split(/\r?\n/)) {
        pushLog(logs.stderr, line);
      }
    });
    child.once("exit", onExit);
    child.once("error", (error) => finish(reject, error));
  });
}

function collectChildLogs(child) {
  const logs = { stdout: [], stderr: [] };
  child.stdout?.on("data", (chunk) => {
    for (const line of chunk.toString().split(/\r?\n/)) {
      pushLog(logs.stdout, line);
    }
  });
  child.stderr?.on("data", (chunk) => {
    for (const line of chunk.toString().split(/\r?\n/)) {
      pushLog(logs.stderr, line);
    }
  });
  return logs;
}

async function waitForHttp(url, child, logs, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(`Vite exited before it was ready\n${formatLogs(logs)}`);
    }
    try {
      const response = await fetch(url, { method: "HEAD" });
      if (response.ok) {
        return;
      }
    } catch {
      // Vite has not bound yet.
    }
    await delay(200);
  }
  throw new Error(`timed out waiting for Vite at ${url}\n${formatLogs(logs)}`);
}

function buildHarnessUrl(baseUrl, options, bridgeUrl) {
  const url = new URL("/tests/ui-harness/harness.html", baseUrl);
  if (options.backend === "live") {
    url.searchParams.set("backend", "live");
    url.searchParams.set("bridgeUrl", bridgeUrl);
  } else {
    url.searchParams.set("scenario", options.scenario);
  }
  return url.toString();
}

function browserContextOptions(viewport, viewportName) {
  if (viewportName === "iphone") {
    return {
      viewport,
      deviceScaleFactor: 3,
      hasTouch: true,
      isMobile: true,
      userAgent:
        "Mozilla/5.0 (iPhone; CPU iPhone OS 26_5 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148",
    };
  }
  return { viewport };
}

async function captureFailure(page, artifactDir, id) {
  try {
    return await captureScreenshot(page, artifactDir, `failure-${id}`, false);
  } catch {
    return null;
  }
}

async function captureScreenshot(page, artifactDir, rawName, fullPage) {
  const name = String(rawName)
    .trim()
    .replace(/[^a-zA-Z0-9._-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 80);
  const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const path = resolve(artifactDir, `${timestamp}-${name || "screenshot"}.png`);
  await mkdir(artifactDir, { recursive: true });
  await page.screenshot({ path, fullPage });
  return path;
}

async function cleanup() {
  if (cleanupPromise) {
    return cleanupPromise;
  }
  cleanupPromise = (async () => {
    await resources.context?.close().catch(() => {});
    await resources.browser?.close().catch(() => {});
    if (resources.live?.child) {
      resources.live.child.stdin.end();
      const exited = await waitForChildExit(resources.live.child, 10_000);
      if (!exited) {
        signalChild(resources.live.child, "SIGTERM", true);
        const terminated = await waitForChildExit(resources.live.child, 3_000);
        if (!terminated) {
          signalChild(resources.live.child, "SIGKILL", true);
          await waitForChildExit(resources.live.child, 5_000);
        }
      }
    }
    if (resources.vite?.child) {
      resources.vite.child.kill("SIGTERM");
      const exited = await waitForChildExit(resources.vite.child, 5_000);
      if (!exited) {
        resources.vite.child.kill("SIGKILL");
        await waitForChildExit(resources.vite.child, 2_000);
      }
    }
    if (resources.mockInference?.server) {
      await new Promise((resolveClose) =>
        resources.mockInference.server.close(resolveClose),
      );
    }
  })();
  return cleanupPromise;
}

function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve(true);
  }
  return new Promise((resolveExit) => {
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      resolveExit(false);
    }, timeoutMs);
    const onExit = () => {
      clearTimeout(timer);
      resolveExit(true);
    };
    child.once("exit", onExit);
  });
}

function signalChild(child, signal, processGroup) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return;
  }
  if (processGroup && process.platform !== "win32" && child.pid) {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch (error) {
      if (error.code !== "ESRCH") {
        throw error;
      }
    }
  }
  child.kill(signal);
}

function parseOptions(argv, env) {
  const values = [...argv];
  const options = {
    apiKeyEnvVar:
      takeFlag(values, "--api-key-env-var") ??
      env.GENTS_TAURI_LIVE_API_KEY_ENV_VAR ??
      null,
    artifactDir: resolve(
      APP_DIR,
      takeFlag(values, "--artifact-dir") ?? "test-results/agent-browser",
    ),
    backend: takeFlag(values, "--backend") ?? "deterministic",
    headed: takeSwitch(values, "--headed"),
    help: takeSwitch(values, "--help") || takeSwitch(values, "-h"),
    inferenceUrl:
      takeFlag(values, "--inference-url") ?? env.GENTS_TAURI_LIVE_INFERENCE_URL ?? null,
    mockResponse:
      takeFlag(values, "--mock-response") ??
      env.GENTS_AGENT_BROWSER_MOCK_RESPONSE ??
      DEFAULT_MOCK_RESPONSE,
    modelName:
      takeFlag(values, "--model-name") ??
      env.GENTS_TAURI_LIVE_MODEL_NAME ??
      DEFAULT_MODEL,
    port: takeFlag(values, "--port"),
    provider:
      takeFlag(values, "--provider") ??
      env.GENTS_TAURI_LIVE_PROVIDER ??
      "openai-compatible",
    scenario: takeFlag(values, "--scenario") ?? "default",
    viewport: takeFlag(values, "--viewport") ?? "iphone",
  };

  if (!["deterministic", "live"].includes(options.backend)) {
    throw new Error(`unsupported backend: ${options.backend}`);
  }
  if (values.length > 0) {
    throw new Error(`unexpected arguments: ${values.join(" ")}`);
  }
  return options;
}

function parseViewport(value) {
  const presets = {
    iphone: { width: 390, height: 844 },
    desktop: { width: 1440, height: 900 },
    laptop: { width: 1280, height: 800 },
  };
  if (presets[value]) {
    return presets[value];
  }
  const match = /^(\d{2,5})x(\d{2,5})$/.exec(value);
  if (!match) {
    throw new Error(
      `invalid viewport "${value}"; use iphone, laptop, desktop, or WIDTHxHEIGHT`,
    );
  }
  return {
    width: clampNumber(Number(match[1]), 240, 8_000, 390),
    height: clampNumber(Number(match[2]), 240, 8_000, 844),
  };
}

function takeFlag(values, name) {
  const directPrefix = `${name}=`;
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (value === name) {
      const next = values[index + 1];
      if (!next || next.startsWith("--")) {
        throw new Error(`missing value for ${name}`);
      }
      values.splice(index, 2);
      return next;
    }
    if (value.startsWith(directPrefix)) {
      values.splice(index, 1);
      return value.slice(directPrefix.length);
    }
  }
  return null;
}

function takeSwitch(values, name) {
  const index = values.indexOf(name);
  if (index < 0) {
    return false;
  }
  values.splice(index, 1);
  return true;
}

function choosePort(rawPort) {
  const requested = rawPort === null ? 0 : Number(rawPort);
  if (!Number.isInteger(requested) || requested < 0 || requested > 65_535) {
    throw new Error(`invalid port: ${rawPort}`);
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

function clampTimeout(value) {
  return clampNumber(value, 1, MAX_WAIT_MS, COMMAND_TIMEOUT_MS);
}

function clampNumber(value, minimum, maximum, fallback) {
  if (value === undefined || value === null || value === "") {
    return fallback;
  }
  const number = Number(value);
  if (!Number.isFinite(number)) {
    throw new Error(`expected a finite number, got ${value}`);
  }
  return Math.min(maximum, Math.max(minimum, Math.trunc(number)));
}

function requireString(value, name) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`${name} must be a non-empty string`);
  }
  return value;
}

function normalizeText(value) {
  return String(value).replace(/\s+/g, " ").trim();
}

function truncate(value, maximum) {
  const text = String(value ?? "");
  if (text.length <= maximum) {
    return text;
  }
  return `${text.slice(0, maximum)}… [truncated ${text.length - maximum} chars]`;
}

function trimEvents(events) {
  while (events.length > 300) {
    events.shift();
  }
}

function pushLog(lines, line) {
  if (!line) {
    return;
  }
  lines.push(truncate(line, 4_000));
  while (lines.length > 80) {
    lines.shift();
  }
}

function formatLogs(logs) {
  return [
    "stdout:",
    logs.stdout.join("\n") || "(empty)",
    "stderr:",
    logs.stderr.join("\n") || "(empty)",
  ].join("\n");
}

function writeJsonLine(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function delay(milliseconds) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));
}

function helpText() {
  return `Gents agent browser

Usage:
  npm run test:ui:agent -- [options]

Options:
  --backend deterministic|live  Adapter layer to drive (default: deterministic)
  --scenario NAME               Deterministic fixture scenario (default: default)
  --viewport PRESET|WIDTHxHEIGHT
                                iphone, laptop, desktop, or an exact size
  --headed                      Show the Chromium window
  --artifact-dir PATH           Screenshot directory
  --port PORT                   Vite port; 0 chooses a free port
  --inference-url URL           Live mode provider endpoint (defaults to local mock)
  --model-name NAME             Live mode model name
  --provider NAME               Live mode provider
  --api-key-env-var NAME        Live mode credential environment variable
  --mock-response TEXT          Local mock's assistant response
  --help                        Show this help

The process writes one JSON ready record, then accepts one JSON command per stdin
line and writes one JSON response per stdout line. Diagnostics go to stderr.`;
}
