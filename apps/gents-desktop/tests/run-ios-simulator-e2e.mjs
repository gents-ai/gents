import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, renameSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const TESTS_ROOT = dirname(fileURLToPath(import.meta.url));
const APP_ROOT = resolve(TESTS_ROOT, "..");
const APPLE_ROOT = join(APP_ROOT, "src-tauri", "gen", "apple");
const APP_BUNDLE_ID = "com.source-inc.gents";
const STATUS_FILENAME = "native-e2e-status.json";
const DEFAULT_TIMEOUT_MS = 10 * 60_000;
const DEFAULT_POST_PASS_STABILITY_MS = 30_000;

const args = new Set(process.argv.slice(2));
const skipBuild = args.has("--skip-build");
const keepData = args.has("--keep-data");
const runsArgument = process.argv.slice(2).find((value) => value.startsWith("--runs="));
const runs = Number.parseInt(runsArgument?.split("=")[1] ?? "1", 10);
if (!Number.isInteger(runs) || runs < 1) {
  throw new Error("--runs must be a positive integer");
}

function run(command, commandArgs, options = {}) {
  const printable = [command, ...commandArgs].join(" ");
  process.stdout.write(`\n$ ${printable}\n`);
  execFileSync(command, commandArgs, {
    cwd: options.cwd ?? APP_ROOT,
    env: options.env ?? process.env,
    stdio: options.stdio ?? "inherit",
    timeout: options.timeout,
  });
}

function capture(command, commandArgs, options = {}) {
  return execFileSync(command, commandArgs, {
    cwd: options.cwd ?? APP_ROOT,
    env: options.env ?? process.env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: options.timeout,
  });
}

function simulator() {
  const requested = process.env.GENTS_IOS_SIMULATOR_ID?.trim();
  const devices = JSON.parse(
    capture("xcrun", ["simctl", "list", "devices", "available", "-j"]),
  ).devices;
  const allDevices = Object.values(devices).flat();
  if (requested) {
    const match = allDevices.find((device) => device.udid === requested);
    if (!match) {
      throw new Error(`GENTS_IOS_SIMULATOR_ID ${requested} is not available`);
    }
    return match;
  }

  return (
    allDevices.find(
      (device) => device.state === "Booted" && device.name.startsWith("iPhone"),
    ) ??
    allDevices.find((device) => device.name === "iPhone 17 Pro") ??
    allDevices.find((device) => device.name.startsWith("iPhone"))
  );
}

function issuerSettings() {
  const supplied = process.env.GENTS_E2E_PAIR_TOKEN?.trim();
  if (supplied) {
    return null;
  }

  const remote = process.env.GENTS_E2E_ISSUER_SSH?.trim();
  const binary =
    process.env.GENTS_E2E_ISSUER_GENTS?.trim() ||
    resolve(APP_ROOT, "..", "..", "target", "release", "gents");
  const home =
    process.env.GENTS_E2E_ISSUER_HOME?.trim() ||
    join(homedir(), ".gents", "iphone-e2e");
  for (const value of [binary, home, ...(remote ? [remote] : [])]) {
    if (!/^[A-Za-z0-9_@./~-]+$/.test(value)) {
      throw new Error(`Unsafe character in E2E issuer setting: ${value}`);
    }
  }
  return { binary, home, remote };
}

function runIssuer(settings, commandArgs) {
  return settings.remote
    ? spawnSync("ssh", [settings.remote, settings.binary, ...commandArgs], {
        encoding: "utf8",
      })
    : spawnSync(settings.binary, commandArgs, { encoding: "utf8" });
}

function issuerFailureDetail(result) {
  return (
    result.error?.message ||
    result.stderr?.trim() ||
    `issuer command exited with status ${result.status ?? "unknown"}`
  );
}

function mintInvite(settings) {
  const supplied = process.env.GENTS_E2E_PAIR_TOKEN?.trim();
  if (supplied) {
    return supplied;
  }
  if (!settings) {
    throw new Error("Native E2E issuer settings are unavailable");
  }
  process.stdout.write(
    `\nMinting a fresh single-use pairing invite from ${
      settings.remote ?? `the local issuer at ${settings.home}`
    }…\n`,
  );
  const inviteArgs = ["p2p", "pairings", "invite", "--home", settings.home, "--bearer"];
  const result = runIssuer(settings, inviteArgs);
  if (result.status !== 0) {
    throw new Error(`Could not mint E2E issuer invite: ${issuerFailureDetail(result)}`);
  }
  const token = result.stdout.match(/dabear1-[1-9A-HJ-NP-Za-km-z]+/)?.[0];
  if (!token) {
    throw new Error("E2E issuer output did not contain a dabear1 token");
  }
  return token;
}

function listIssuerPairings(settings) {
  if (!settings) {
    return [];
  }
  const result = runIssuer(settings, [
    "p2p",
    "pairings",
    "list",
    "--home",
    settings.home,
    "--output",
    "json",
  ]);
  if (result.status !== 0) {
    throw new Error(
      `Could not list E2E issuer pairings: ${issuerFailureDetail(result)}`,
    );
  }
  const parsed = JSON.parse(result.stdout);
  return Array.isArray(parsed.pairings) ? parsed.pairings : [];
}

function cleanNewIssuerPairings(settings, peerIdsBefore) {
  if (!settings || settings.remote) {
    return;
  }

  let pairings;
  try {
    pairings = listIssuerPairings(settings).filter(
      (pairing) => !peerIdsBefore.has(pairing.peer_id),
    );
  } catch (error) {
    process.stderr.write(`Native E2E pairing cleanup warning: ${error.message}\n`);
    return;
  }

  for (const pairing of pairings) {
    process.stdout.write(`Cleaning E2E pairing ${pairing.peer_id}…\n`);
    if (pairing.agent_did) {
      const revoke = runIssuer(settings, [
        "p2p",
        "network",
        "revoke",
        "--home",
        settings.home,
        pairing.agent_did,
        "--output",
        "json",
      ]);
      if (revoke.status !== 0) {
        process.stderr.write(
          `Native E2E membership cleanup warning: ${issuerFailureDetail(revoke)}\n`,
        );
      }
    }
    const remove = runIssuer(settings, [
      "p2p",
      "pairings",
      "rm",
      "--home",
      settings.home,
      "--peer",
      pairing.peer_id,
    ]);
    if (remove.status !== 0) {
      process.stderr.write(
        `Native E2E pairing cleanup warning: ${issuerFailureDetail(remove)}\n`,
      );
    }
  }
}

function readStatus(statusPath) {
  if (!existsSync(statusPath)) {
    return null;
  }
  try {
    return JSON.parse(readFileSync(statusPath, "utf8"));
  } catch {
    return null;
  }
}

function processIsAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function captureScreenshot({ deviceId, path }) {
  const result = spawnSync("xcrun", ["simctl", "io", deviceId, "screenshot", path], {
    encoding: "utf8",
    timeout: 30_000,
  });
  if (result.status === 0) {
    return;
  }

  const detail =
    result.error?.message ||
    result.stderr?.trim() ||
    `simctl exited with status ${result.status ?? "unknown"}`;
  process.stderr.write(`Simulator screenshot warning: ${detail}\n`);
}

async function waitForScenario({ deviceId, pid, runIndex, artifactRoot, statusPath }) {
  const timeoutMs = Number.parseInt(
    process.env.GENTS_E2E_TIMEOUT_MS ?? `${DEFAULT_TIMEOUT_MS}`,
    10,
  );
  const stabilityMs = Number.parseInt(
    process.env.GENTS_E2E_STABILITY_MS ?? `${DEFAULT_POST_PASS_STABILITY_MS}`,
    10,
  );
  const deadline = Date.now() + timeoutMs;
  let lastStage = null;
  let passedAt = null;

  while (Date.now() < deadline) {
    const status = readStatus(statusPath);
    if (status?.stage && status.stage !== lastStage) {
      lastStage = status.stage;
      process.stdout.write(
        `Native app stage: ${status.stage}${status.detail ? ` · ${status.detail}` : ""}\n`,
      );
      if (["sent", "passed", "failed"].includes(status.stage)) {
        const screenshot = join(artifactRoot, `run-${runIndex}-${status.stage}.png`);
        captureScreenshot({ deviceId, path: screenshot });
      }
      if (status.stage === "passed") {
        passedAt ??= Date.now();
      }
      if (status.stage === "failed") {
        throw new Error(`Native app E2E failed: ${status.detail ?? "unknown"}`);
      }
    }

    if (!processIsAlive(pid)) {
      throw new Error(
        `Gents exited during native E2E${
          lastStage ? ` at stage ${lastStage}` : ""
        }${passedAt ? " during the post-pass stability window" : ""}`,
      );
    }
    if (passedAt && Date.now() - passedAt >= stabilityMs) {
      process.stdout.write(
        `Native app remained alive for ${stabilityMs}ms after the response.\n`,
      );
      return;
    }
    await delay(500);
  }

  throw new Error(
    `Native app E2E timed out${lastStage ? ` at stage ${lastStage}` : ""}`,
  );
}

function delay(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

const device = simulator();
if (!device) {
  throw new Error("No available iPhone Simulator was found");
}
process.stdout.write(`Using ${device.name} (${device.udid})\n`);

if (device.state !== "Booted") {
  run("xcrun", ["simctl", "boot", device.udid]);
}
run("open", ["-a", "Simulator"]);
run("xcrun", ["simctl", "bootstatus", device.udid, "-b"]);

const artifactRoot = mkdtempSync(join(tmpdir(), "gents-ios-e2e-"));
const appBundle =
  process.env.GENTS_IOS_APP_BUNDLE?.trim() ||
  join(APPLE_ROOT, "build", "arm64-sim", "Gents.app");

if (!skipBuild) {
  run("npm", ["run", "build"]);
  const priorTauriBuild = join(APPLE_ROOT, "build");
  if (existsSync(priorTauriBuild)) {
    renameSync(priorTauriBuild, join(artifactRoot, "prior-tauri-build"));
  }
  run(
    "npm",
    [
      "run",
      "tauri",
      "--",
      "ios",
      "build",
      "--debug",
      "--target",
      "aarch64-sim",
      "--ci",
    ],
    { cwd: APP_ROOT },
  );
}

if (!existsSync(appBundle)) {
  throw new Error(`Simulator app bundle does not exist: ${appBundle}`);
}

if (!keepData) {
  const uninstall = spawnSync(
    "xcrun",
    ["simctl", "uninstall", device.udid, APP_BUNDLE_ID],
    { encoding: "utf8" },
  );
  if (
    uninstall.status !== 0 &&
    !uninstall.stderr.includes("No such file") &&
    !uninstall.stderr.includes("not installed")
  ) {
    process.stderr.write(`Simulator reset warning: ${uninstall.stderr}`);
  }
}

run("xcrun", ["simctl", "install", device.udid, appBundle]);
const dataContainer = capture("xcrun", [
  "simctl",
  "get_app_container",
  device.udid,
  APP_BUNDLE_ID,
  "data",
]).trim();
const statusPath = join(dataContainer, "tmp", STATUS_FILENAME);
const managedIssuer = issuerSettings();
const issuerPeerIdsBefore = new Set(
  listIssuerPairings(managedIssuer).map((pairing) => pairing.peer_id),
);
const invite = mintInvite(managedIssuer);

try {
  for (let index = 1; index <= runs; index += 1) {
    rmSync(statusPath, { force: true });
    const defaultPrompt = `Reply with only the uppercase underscore form of: isolated iphone simulator e2e run ${index}.`;
    const defaultExpected = `ISOLATED_IPHONE_SIMULATOR_E2E_RUN_${index}`;
    const launchEnvironment = {
      ...process.env,
      SIMCTL_CHILD_GENTS_NATIVE_E2E: "1",
      SIMCTL_CHILD_GENTS_E2E_AGENT_LABEL:
        process.env.GENTS_E2E_AGENT_LABEL?.trim() || "iPhone E2E",
      SIMCTL_CHILD_GENTS_E2E_PAIR_TOKEN: invite,
      SIMCTL_CHILD_GENTS_E2E_PROMPT:
        process.env.GENTS_E2E_PROMPT?.trim() || defaultPrompt,
      SIMCTL_CHILD_GENTS_E2E_EXPECTED_RESPONSE:
        process.env.GENTS_E2E_EXPECTED_RESPONSE?.trim() || defaultExpected,
      SIMCTL_CHILD_GENTS_E2E_EXPECT_EMPTY_CONVERSATIONS:
        index === 1 && !keepData ? "1" : "0",
      SIMCTL_CHILD_RUST_BACKTRACE: "full",
      SIMCTL_CHILD_RUST_LOG: process.env.RUST_LOG?.trim() || "info",
    };

    process.stdout.write(`\nRunning isolated prompt round-trip ${index}/${runs}…\n`);
    const launchResult = capture(
      "xcrun",
      ["simctl", "launch", "--terminate-running-process", device.udid, APP_BUNDLE_ID],
      { env: launchEnvironment },
    );
    const pid = Number.parseInt(launchResult.match(/: (\d+)\s*$/)?.[1] ?? "", 10);
    if (!Number.isInteger(pid)) {
      throw new Error(`Could not parse Gents simulator PID from ${launchResult}`);
    }

    await waitForScenario({
      deviceId: device.udid,
      pid,
      runIndex: index,
      artifactRoot,
      statusPath,
    });
  }
} finally {
  cleanNewIssuerPairings(managedIssuer, issuerPeerIdsBefore);
}

process.stdout.write(`\nIsolated iPhone Simulator E2E passed ${runs} run(s).\n`);
process.stdout.write(`Result screenshots: ${artifactRoot}\n`);
