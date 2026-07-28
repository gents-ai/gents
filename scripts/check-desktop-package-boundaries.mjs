#!/usr/bin/env node

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const root = new URL("../", import.meta.url).pathname;
const packageNames = [
  "gents-desktop-tokens",
  "gents-desktop-client",
  "gents-desktop-ui",
  "gents-desktop-chat",
  "gents-desktop-fleet",
  "gents-desktop-operations",
];
const allowedImports = new Map([
  ["gents-desktop-tokens", new Set()],
  ["gents-desktop-client", new Set()],
  ["gents-desktop-ui", new Set(["gents-desktop-tokens"])],
  ["gents-desktop-chat", new Set(["gents-desktop-client", "gents-desktop-ui"])],
  [
    "gents-desktop-fleet",
    new Set(["gents-desktop-client", "gents-desktop-ui"]),
  ],
  [
    "gents-desktop-operations",
    new Set(["gents-desktop-client", "gents-desktop-ui"]),
  ],
]);

function filesUnder(directory, predicate) {
  if (!statSync(directory, { throwIfNoEntry: false })?.isDirectory()) return [];
  return readdirSync(directory).flatMap((entry) => {
    const path = join(directory, entry);
    return statSync(path).isDirectory()
      ? filesUnder(path, predicate)
      : predicate(path)
        ? [path]
        : [];
  });
}

const failures = [];
const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");
const workspaceVersion = cargo.match(
  /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
)?.[1];
if (!workspaceVersion) {
  failures.push("Could not read workspace.package.version from Cargo.toml");
}

const manifests = new Map();
for (const name of packageNames) {
  const manifestPath = join(root, "packages", name, "package.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  manifests.set(name, manifest);
  if (manifest.version !== workspaceVersion) {
    failures.push(
      `${relative(root, manifestPath)} version ${manifest.version} != ${workspaceVersion}`,
    );
  }
  for (const section of [
    "dependencies",
    "peerDependencies",
    "devDependencies",
  ]) {
    for (const [dependency, version] of Object.entries(
      manifest[section] ?? {},
    )) {
      if (
        dependency.startsWith("@source-inc/gents-desktop-") &&
        version !== workspaceVersion
      ) {
        failures.push(
          `${relative(root, manifestPath)} ${section}.${dependency} must be exactly ${workspaceVersion}`,
        );
      }
    }
  }
}

for (const app of [
  "package.json",
  "apps/gents-desktop/package.json",
  "apps/fixture-host/package.json",
]) {
  const manifest = JSON.parse(readFileSync(join(root, app), "utf8"));
  if (manifest.version !== workspaceVersion) {
    failures.push(`${app} version ${manifest.version} != ${workspaceVersion}`);
  }
  for (const [dependency, version] of Object.entries(
    manifest.dependencies ?? {},
  )) {
    if (
      dependency.startsWith("@source-inc/gents-desktop-") &&
      version !== workspaceVersion
    ) {
      failures.push(
        `${app} pins ${dependency} to ${version}, expected ${workspaceVersion}`,
      );
    }
  }
}

for (const name of packageNames) {
  const sourceRoot = join(root, "packages", name, "src");
  for (const file of filesUnder(sourceRoot, (path) =>
    /\.[cm]?[jt]sx?$/.test(path),
  )) {
    const source = readFileSync(file, "utf8");
    if (/(?:from\s+|import\s*\()["'][^"']*apps\/gents-desktop/.test(source)) {
      failures.push(`${relative(root, file)} imports from the host app`);
    }
    for (const match of source.matchAll(
      /(?:from\s+|import\s*\()["']@source-inc\/(gents-desktop-[^/"']+)/g,
    )) {
      const dependency = match[1];
      if (!allowedImports.get(name)?.has(dependency)) {
        failures.push(
          `${relative(root, file)} crosses package boundary ${name} -> ${dependency}`,
        );
      }
    }
  }

  for (const file of filesUnder(join(root, "packages", name), (path) =>
    path.endsWith(".css"),
  )) {
    const css = readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    if (css.includes("--source-")) {
      failures.push(
        `${relative(root, file)} references a host-private --source-* token`,
      );
    }
  }
}

if (failures.length > 0) {
  console.error(failures.map((failure) => `- ${failure}`).join("\n"));
  process.exit(1);
}

console.log(
  `Desktop package boundaries and lockstep version ${workspaceVersion} are valid.`,
);
