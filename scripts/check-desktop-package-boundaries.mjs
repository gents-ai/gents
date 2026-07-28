#!/usr/bin/env node

import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
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
const dependencySections = [
  "dependencies",
  "peerDependencies",
  "devDependencies",
  "optionalDependencies",
];
const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");
const workspaceVersion = cargo.match(
  /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
)?.[1];
if (!workspaceVersion) {
  failures.push("Could not read workspace.package.version from Cargo.toml");
}

function checkInternalVersions(manifest, manifestPath) {
  for (const section of dependencySections) {
    for (const [dependency, version] of Object.entries(
      manifest[section] ?? {},
    )) {
      if (
        dependency.startsWith("@source-inc/gents-desktop-") &&
        version !== workspaceVersion
      ) {
        failures.push(
          `${manifestPath} ${section}.${dependency} must be exactly ${workspaceVersion}`,
        );
      }
    }
  }

  function visitOverrides(value, path) {
    if (!value || typeof value !== "object" || Array.isArray(value)) return;
    for (const [dependency, override] of Object.entries(value)) {
      const nextPath = `${path}.${dependency}`;
      if (
        dependency.startsWith("@source-inc/gents-desktop-") &&
        typeof override === "string" &&
        override !== workspaceVersion
      ) {
        failures.push(
          `${manifestPath} ${nextPath} must be exactly ${workspaceVersion}`,
        );
      }
      visitOverrides(override, nextPath);
    }
  }

  visitOverrides(manifest.overrides, "overrides");
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
  checkInternalVersions(manifest, relative(root, manifestPath));
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
  checkInternalVersions(manifest, app);
}

const clientSourcePath = join(
  root,
  "packages/gents-desktop-client/src/client.ts",
);
const clientSource = readFileSync(clientSourcePath, "utf8");
const clientPackageVersion = clientSource.match(
  /export const PACKAGE_VERSION = "([^"]+)";/,
)?.[1];
if (clientPackageVersion !== workspaceVersion) {
  failures.push(
    `${relative(root, clientSourcePath)} PACKAGE_VERSION ${clientPackageVersion ?? "<missing>"} != ${workspaceVersion}`,
  );
}

for (const configPath of [
  "apps/gents-desktop/src-tauri/tauri.conf.json",
  "apps/fixture-host/src-tauri/tauri.conf.json",
]) {
  const config = JSON.parse(readFileSync(join(root, configPath), "utf8"));
  if (config.version !== workspaceVersion) {
    failures.push(
      `${configPath} version ${config.version ?? "<missing>"} != ${workspaceVersion}`,
    );
  }
}

for (const name of packageNames) {
  const packageRoot = join(root, "packages", name);
  const sourceRoot = join(root, "packages", name, "src");
  for (const file of filesUnder(sourceRoot, (path) =>
    /\.[cm]?[jt]sx?$/.test(path),
  )) {
    const source = readFileSync(file, "utf8");
    if (
      /(?:from\s+|import\s*\()["'][^"']*apps\/(?:gents-desktop|fixture-host)/.test(
        source,
      )
    ) {
      failures.push(`${relative(root, file)} imports from the host app`);
    }
    for (const match of source.matchAll(
      /(?:from\s+|import\s*\()\s*["']([^"']+)["']/g,
    )) {
      const specifier = match[1];
      if (!specifier.startsWith(".")) continue;
      const target = resolve(dirname(file), specifier);
      if (
        target !== packageRoot &&
        !target.startsWith(`${packageRoot}${sep}`)
      ) {
        failures.push(
          `${relative(root, file)} escapes its package via ${specifier}`,
        );
      }
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
