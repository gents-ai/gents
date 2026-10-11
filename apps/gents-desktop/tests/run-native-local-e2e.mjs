import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { nativeLocalHelp, parseNativeLocalOptions } from "./native-local-options.mjs";

import { createNativeProviderGate } from "./native-local-provider-gate.mjs";

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(appRoot, "../..");
const options = parseNativeLocalOptions(process.argv.slice(2), process.env);
if (options.help) {
  console.log(nativeLocalHelp);
  process.exit(0);
}
if (process.platform !== "darwin")
  throw new Error("Native local acceptance currently requires macOS launchd.");
const service = "ai.gents.runtime.native-e2e";
const serviceTarget = `gui/${process.getuid()}/${service}`;
const definition = join(homedir(), "Library", "LaunchAgents", `${service}.plist`);
const target = resolve(repoRoot, process.env.CARGO_TARGET_DIR ?? "target", "debug");
const artifactRoot = await mkdtemp(join(tmpdir(), "gents-native-local-"));
const nodeHome = join(artifactRoot, "node");
const desktopHome = join(artifactRoot, "desktop");
const toolRoot = join(artifactRoot, "workspace");
const runTmp = join(artifactRoot, "tmp");
const webviewStoreId = randomBytes(16).toString("hex");
const binaries = join(artifactRoot, "bin");
for (const directory of [nodeHome, desktopHome, toolRoot, runTmp, binaries]) {
  await mkdir(directory);
}
console.log(`Native acceptance artifacts: ${artifactRoot}`);
await writeFile(
  "/tmp/gents-native-local-latest.json",
  JSON.stringify({
    artifactRoot,
    nodeHome,
    desktopHome,
    toolRoot,
    serviceTarget,
    webviewStoreId,
    storeKeyCustody: options.fileStoreKeys ? "file" : "macos-keychain",
  }),
);
const buildLog = createWriteStream(join(artifactRoot, "build.log"));
let app = null;
let log = null;
let passed = false;
let providerGate = null;

async function recordedCustody() {
  const node = JSON.parse(await readFile(join(nodeHome, "init.json"), "utf8"));
  const desktop = JSON.parse(
    await readFile(join(desktopHome, "store-encryption.json"), "utf8"),
  );
  const records = { node: node.store_encryption, desktop };
  const expected = options.fileStoreKeys ? "file" : "macos-keychain";
  for (const record of Object.values(records)) {
    if (record?.custody !== expected)
      throw new Error(`Expected native store custody ${expected}`);
  }
  return records;
}

async function command(binary, argv, options = {}) {
  const child = spawn(binary, argv, {
    cwd: repoRoot,
    ...options,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  child.stdout.on("data", (chunk) => {
    output += chunk;
    options.log?.write(chunk);
  });
  child.stderr.on("data", (chunk) => {
    output += chunk;
    options.log?.write(chunk);
  });
  const code = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", resolve);
  });
  return { code, output };
}

async function checked(binary, argv, options) {
  const result = await command(binary, argv, options);
  if (result.code !== 0)
    throw new Error(
      `${binary} ${argv.join(" ")} failed (${result.code}): ${result.output.slice(-4000)}`,
    );
  return result;
}

async function requireIsolationMarkers(name, markers) {
  const missing = new Set(markers);
  let tail = "";
  for await (const chunk of createReadStream(join(binaries, name))) {
    const text = tail + chunk.toString("latin1");
    for (const marker of missing) if (text.includes(marker)) missing.delete(marker);
    if (missing.size === 0) return;
    tail = text.slice(-128);
  }
  throw new Error(
    `Refusing ${name} without native isolation markers: ${[...missing].join(", ")}`,
  );
}

async function stopApp() {
  if (app && app.exitCode === null && app.signalCode === null) {
    app.kill("SIGTERM");
    await Promise.race([
      new Promise((resolve) => app.once("exit", resolve)),
      new Promise((resolve) => setTimeout(resolve, 5000)),
    ]);
    if (app.exitCode === null && app.signalCode === null) {
      app.kill("SIGKILL");
      await new Promise((resolve) => app.once("exit", resolve));
    }
  }
  log?.end();
  app = null;
}

async function runPhase(phase) {
  const statusPath = join(runTmp, "native-e2e-status.json");
  await rm(statusPath, { force: true });
  const marker = `NATIVE_ENGINEER_${phase.toUpperCase()}_CONFIRMED`;
  const prompt = `Reply with exactly ${marker}. Do not call tools.`;
  providerGate.arm(prompt);
  const env = {
    ...process.env,
    TMPDIR: `${runTmp}/`,
    GENTS_BIN: join(binaries, "gents"),
    GENTS_NATIVE_E2E: "1",
    GENTS_E2E_FILE_STORE_KEYS: options.fileStoreKeys ? "1" : "0",
    GENTS_DESKTOP_CONSOLE_LOG: "1",
    GENTS_E2E_NODE_HOME: nodeHome,
    GENTS_E2E_DESKTOP_HOME: desktopHome,
    GENTS_E2E_LOCAL_TOOL_ROOT: toolRoot,
    GENTS_E2E_AGENT_LABEL: "Native Acceptance Engineer",
    GENTS_E2E_SERVER_ADDRESS: providerGate.endpoint,
    GENTS_E2E_LOCAL_MODEL:
      process.env.GENTS_TAURI_LIVE_MODEL_NAME ?? "GLM-5.3-Flash-NVFP4",
    GENTS_E2E_LOCAL_PHASE: phase,
    GENTS_E2E_WEBVIEW_STORE_ID: webviewStoreId,
    GENTS_E2E_PROMPT: prompt,
    GENTS_E2E_EXPECTED_RESPONSE: marker,
  };
  log = createWriteStream(join(artifactRoot, `${phase}.log`));
  app = spawn(join(binaries, "gents-desktop-tauri"), [], {
    cwd: repoRoot,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  app.stdout.pipe(log, { end: false });
  app.stderr.pipe(log, { end: false });
  let spawnError;
  app.once("error", (error) => {
    spawnError = error;
  });
  const deadline = Date.now() + 15 * 60_000;
  let lastStage = "";
  while (Date.now() < deadline) {
    if (spawnError) throw spawnError;
    if (app.exitCode !== null || app.signalCode !== null)
      throw new Error(`Native app exited during ${phase}; see ${phase}.log`);
    let status;
    try {
      status = JSON.parse(await readFile(statusPath, "utf8"));
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    if (status?.stage !== lastStage && status) {
      lastStage = status.stage;
      console.log(
        `${phase}: ${status.stage}${status.detail ? ` — ${status.detail}` : ""}`,
      );
    }
    if (status?.stage === "failed")
      throw new Error(status.detail ?? "Native acceptance failed");
    if (status?.stage === `local-${phase}-passed`) {
      await writeFile(
        join(artifactRoot, `${phase}-result.json`),
        JSON.stringify(status, null, 2),
      );
      const userText = (capture) =>
        capture.messages
          .filter((message) => message.role === "user")
          .map((message) =>
            typeof message.content === "string"
              ? message.content
              : JSON.stringify(message.content),
          )
          .join("\n");
      const amended = `QUEUE_${phase}_EDITED`;
      const retained = `QUEUE_${phase}_SECOND`;
      for (const capture of providerGate.captures) {
        const text = userText(capture);
        if (
          text.includes(`QUEUE_${phase}_ORIGINAL`) ||
          text.includes(`QUEUE_${phase}_REMOVED`)
        )
          throw new Error(
            "Real provider input included an obsolete or removed queue message",
          );
      }
      const matched = providerGate.captures.find((capture) => {
        const text = userText(capture);
        return text.includes(amended) && text.includes(retained);
      });
      if (!matched)
        throw new Error(
          "No real subsequent provider request contained both retained queue inputs",
        );
      const text = userText(matched);
      if (
        text.includes(`QUEUE_${phase}_ORIGINAL`) ||
        text.includes(`QUEUE_${phase}_REMOVED`) ||
        text.indexOf(retained) > text.indexOf(amended)
      )
        throw new Error(
          "Real provider input did not preserve queue edit/remove/reorder",
        );
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Native ${phase} timed out; see ${phase}.log`);
}

try {
  const macos = await checked("sw_vers", ["-productVersion"]);
  if (Number.parseInt(macos.output, 10) < 14) {
    throw new Error("Isolated native WebView storage requires macOS 14 or later");
  }
  if ((await command("launchctl", ["print", serviceTarget])).code === 0)
    throw new Error(`Refusing to replace running ${serviceTarget}`);
  try {
    await readFile(definition);
    throw new Error(`Refusing to replace existing ${definition}`);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  await new Promise((resolve, reject) => {
    const probe = createServer();
    probe.once("error", reject);
    probe.listen(21919, "127.0.0.1", () => probe.close(resolve));
  });
  if (!options.skipBuild) {
    const env = { ...process.env, CARGO_BUILD_JOBS: "3", VITE_GENTS_NATIVE_E2E: "1" };
    console.log("Building isolated native-e2e runtime and desktop");
    await checked(
      "cargo",
      ["build", "-p", "gents-cli", "--bin", "gents", "--features", "native-e2e"],
      { env, log: buildLog },
    );
    await checked(
      "npm",
      [
        "run",
        "tauri",
        "--",
        "build",
        "--debug",
        "--no-bundle",
        "--features",
        "native-e2e",
        "--config",
        "src-tauri/tauri.local-e2e.conf.json",
      ],
      { cwd: appRoot, env, log: buildLog },
    );
  }
  // Cargo validation can replace target/debug executables during the restart.
  // This run must keep using the exact binaries whose setup it exercised.
  for (const name of ["gents", "gents-desktop-tauri"]) {
    const source =
      name === "gents" && process.env.GENTS_BIN
        ? process.env.GENTS_BIN
        : join(target, name);
    await copyFile(source, join(binaries, name));
  }
  await requireIsolationMarkers("gents-desktop-tauri", [
    service,
    "com.source-inc.gents.local-e2e",
  ]);
  await requireIsolationMarkers("gents", [service]);
  providerGate = await createNativeProviderGate(
    process.env.GENTS_TAURI_LIVE_INFERENCE_URL ?? "http://workstation-1:8000/v1",
    join(artifactRoot, "provider-requests.jsonl"),
  );
  await runPhase("setup");
  const custody = await recordedCustody();
  const beforeRestart = await checked("launchctl", ["print", serviceTarget]);
  const beforePid = beforeRestart.output.match(/^\s*pid = (\d+)/m)?.[1];
  if (!beforePid || Number(beforePid) === app.pid) {
    throw new Error("Native acceptance did not run a separate managed runtime process");
  }
  const firstAppPid = app.pid;
  await stopApp();
  await checked("launchctl", ["kickstart", "-k", serviceTarget]);
  await runPhase("reopen");
  if (JSON.stringify(custody) !== JSON.stringify(await recordedCustody())) {
    throw new Error("Native restart replaced recorded store-key custody");
  }
  await writeFile(
    join(artifactRoot, "store-custody.json"),
    JSON.stringify(custody, null, 2),
  );
  const afterRestart = await checked("launchctl", ["print", serviceTarget]);
  const afterPid = afterRestart.output.match(/^\s*pid = (\d+)/m)?.[1];
  if (!afterPid || afterPid === beforePid || app.pid === firstAppPid) {
    throw new Error(
      "Native acceptance did not restart both desktop and runtime processes",
    );
  }
  await writeFile(
    join(artifactRoot, "process-restart.json"),
    JSON.stringify(
      {
        serviceTarget,
        runtimeBefore: Number(beforePid),
        runtimeAfter: Number(afterPid),
        desktopBefore: firstAppPid,
        desktopAfter: app.pid,
      },
      null,
      2,
    ),
  );
  console.log(
    `Native setup and runtime/desktop restart passed. Evidence: ${artifactRoot}`,
  );
  passed = true;
} finally {
  await stopApp();
  await providerGate?.close();
  // Remove only the isolated service definition created for this run's home.
  const contents = await readFile(definition, "utf8").catch(() => "");
  if (contents.includes(nodeHome)) {
    await command("launchctl", ["bootout", serviceTarget]);
    await rm(definition);
  }
  if (passed) {
    for (const record of Object.values(await recordedCustody())) {
      if (
        record.custody === "macos-keychain" &&
        /^store-[a-f0-9]{32}$/.test(record.keychain_label)
      ) {
        await checked("security", [
          "delete-generic-password",
          "-s",
          "com.source-inc.gents.store-key",
          "-a",
          record.keychain_label,
        ]);
      }
    }
  }
  buildLog.end();
}
