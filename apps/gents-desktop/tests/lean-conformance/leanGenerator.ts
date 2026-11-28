import { spawn } from "node:child_process";
import { existsSync, statSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { dirname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";

const CONTRACT_JSON_BEGIN = "---BEGIN GENTS LEAN CONTRACT JSON---";
const CONTRACT_JSON_END = "---END GENTS LEAN CONTRACT JSON---";
const CONTRACT_TARGET = "Proofs.Conformance.Contracts:olean";
const CONTRACT_FILE = "Proofs/Conformance/Contracts.lean";
const DEFAULT_TIMEOUT_MS = 240_000;
const BINARY_PROFILES = ["ci-native", "fast-dev", "debug", "release"] as const;

export type LeanContractSnapshot = {
  families: Record<string, { cases: unknown[] }>;
};

let cachedRepoRoot: string | null = null;
let cachedContractSnapshot: Promise<LeanContractSnapshot> | null = null;

function findRepoRoot(): string {
  if (cachedRepoRoot) return cachedRepoRoot;
  let dir = dirname(fileURLToPath(import.meta.url));
  const { root } = parse(dir);
  while (dir !== root) {
    if (existsSync(join(dir, "crates/gents/proofs/lakefile.lean"))) {
      cachedRepoRoot = dir;
      return dir;
    }
    dir = dirname(dir);
  }
  throw new Error("could not locate repository root from leanGenerator.ts");
}

function findPrebuiltGenerator(repoRoot: string): string | null {
  const override = process.env.GENTS_LEAN_GENERATOR?.trim();
  if (override) {
    if (!existsSync(override)) {
      throw new Error(`GENTS_LEAN_GENERATOR points to missing binary: ${override}`);
    }
    return override;
  }
  const binaryName =
    process.platform === "win32" ? "gents-lean-generator.exe" : "gents-lean-generator";
  const targetRoots = [
    process.env.CARGO_TARGET_DIR?.trim(),
    join(repoRoot, "target"),
  ].filter((value): value is string => Boolean(value));

  let newest: { path: string; mtimeMs: number } | null = null;
  for (const targetRoot of targetRoots) {
    for (const profile of BINARY_PROFILES) {
      const candidate = join(targetRoot, profile, binaryName);
      if (!existsSync(candidate)) continue;
      const mtimeMs = statSync(candidate).mtimeMs;
      if (!newest || mtimeMs > newest.mtimeMs) {
        newest = { path: candidate, mtimeMs };
      }
    }
  }
  return newest?.path ?? null;
}

function execAsync(
  command: string,
  args: string[],
  cwd: string,
  timeoutMs: number,
): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
      timeout: timeoutMs,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.on("error", (error) => {
      reject(new Error(`failed to run ${command} ${args.join(" ")}: ${error.message}`));
    });
    child.on("close", (status) => {
      if (status !== 0) {
        reject(
          new Error(
            `${command} ${args.join(" ")} failed (exit ${status})\nstdout:\n${stdout}\nstderr:\n${stderr}`,
          ),
        );
        return;
      }
      resolve(stdout);
    });
  });
}

function extractContractJson(raw: string, allowRawJson: boolean): string {
  const firstBegin = raw.indexOf(CONTRACT_JSON_BEGIN);
  if (firstBegin < 0) {
    if (allowRawJson) return raw.trim();
    throw new Error(
      `Lean contract generator stdout did not contain ${CONTRACT_JSON_BEGIN}`,
    );
  }
  if (raw.lastIndexOf(CONTRACT_JSON_BEGIN) !== firstBegin) {
    throw new Error(
      `Lean contract generator stdout contained duplicate ${CONTRACT_JSON_BEGIN} sentinels`,
    );
  }
  const firstEnd = raw.indexOf(CONTRACT_JSON_END);
  if (firstEnd < 0 || raw.lastIndexOf(CONTRACT_JSON_END) !== firstEnd) {
    throw new Error(
      `Lean contract generator stdout had missing or duplicate ${CONTRACT_JSON_END} sentinels`,
    );
  }
  if (firstBegin >= firstEnd) {
    throw new Error("Lean contract JSON sentinel order is invalid");
  }
  return raw.slice(firstBegin + CONTRACT_JSON_BEGIN.length, firstEnd).trim();
}

export async function runLeanGenerator(
  target: string,
  file: string,
  timeoutMs = DEFAULT_TIMEOUT_MS,
): Promise<string> {
  if (target === CONTRACT_TARGET && file === CONTRACT_FILE) {
    const contractFile = process.env.GENTS_LEAN_CONTRACT_FILE?.trim();
    if (contractFile) {
      return readFile(contractFile, "utf8");
    }
  }
  const repoRoot = findRepoRoot();
  const proofsDir = join(repoRoot, "crates/gents/proofs");
  const binary = findPrebuiltGenerator(repoRoot);
  if (binary) {
    return execAsync(binary, [proofsDir, target, file], repoRoot, timeoutMs);
  }
  return execAsync(
    "cargo",
    [
      "run",
      "--quiet",
      "--profile",
      "fast-dev",
      "-p",
      "gents-lean-contract",
      "--bin",
      "gents-lean-generator",
      "--",
      proofsDir,
      target,
      file,
    ],
    repoRoot,
    timeoutMs,
  );
}

export async function runLeanGeneratorJson<T>(
  target: string,
  file: string,
  timeoutMs = DEFAULT_TIMEOUT_MS,
): Promise<T> {
  const stdout = await runLeanGenerator(target, file, timeoutMs);
  return JSON.parse(stdout.trim()) as T;
}

export function loadLeanContractSnapshot(): Promise<LeanContractSnapshot> {
  cachedContractSnapshot ??= (async () => {
    const contractFile = process.env.GENTS_LEAN_CONTRACT_FILE?.trim();
    const raw = contractFile
      ? await readFile(contractFile, "utf8")
      : await runLeanGenerator(CONTRACT_TARGET, CONTRACT_FILE);
    return JSON.parse(
      extractContractJson(raw, Boolean(contractFile)),
    ) as LeanContractSnapshot;
  })();
  return cachedContractSnapshot;
}

export async function loadLeanFamilyCases<Case>(familyPath: string): Promise<Case[]> {
  const key = /^families::(\w+)::cases\(\)$/.exec(familyPath)?.[1];
  if (!key) {
    throw new Error(`not a Lean family path: ${familyPath}`);
  }
  const family = (await loadLeanContractSnapshot()).families[key];
  if (!family) {
    throw new Error(`Lean family ${key} was not emitted`);
  }
  return family.cases as Case[];
}
