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
  [
    "gents-desktop-chat",
    new Set([
      "gents-desktop-client",
      "gents-desktop-tokens",
      "gents-desktop-ui",
    ]),
  ],
  [
    "gents-desktop-fleet",
    new Set([
      "gents-desktop-client",
      "gents-desktop-tokens",
      "gents-desktop-ui",
    ]),
  ],
  [
    "gents-desktop-operations",
    new Set([
      "gents-desktop-client",
      "gents-desktop-tokens",
      "gents-desktop-ui",
    ]),
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

for (const name of [
  "gents-desktop-chat",
  "gents-desktop-fleet",
  "gents-desktop-operations",
]) {
  const manifest = manifests.get(name);
  for (const section of ["peerDependencies", "devDependencies"]) {
    if (
      manifest?.[section]?.["@source-inc/gents-desktop-tokens"] !==
      workspaceVersion
    ) {
      failures.push(
        `packages/${name}/package.json ${section} must declare @source-inc/gents-desktop-tokens ${workspaceVersion}`,
      );
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

const uiStylesPath = join(root, "packages/gents-desktop-ui/styles.css");
const uiStyles = readFileSync(uiStylesPath, "utf8");
for (const className of [
  "primary-button",
  "ghost-button",
  "danger-button",
  "icon-button",
  "panel",
  "muted",
  "eyebrow",
  "field",
  "mono",
  "small",
  "chip",
]) {
  if (!new RegExp(`\\.${className}(?:[\\s:,.{]|$)`).test(uiStyles)) {
    failures.push(
      `${relative(root, uiStylesPath)} must own shared .${className} primitive`,
    );
  }
}

const chatStylesPath = join(
  root,
  "packages/gents-desktop-chat/styles/chat.css",
);
const chatStyles = readFileSync(chatStylesPath, "utf8");
for (const className of [
  "chat-header",
  "chat-status",
  "composer-footer",
  "empty-transcript",
]) {
  if (!new RegExp(`\\.${className}(?:[\\s:,.{]|$)`).test(chatStyles)) {
    failures.push(
      `${relative(root, chatStylesPath)} must own package-emitted .${className}`,
    );
  }
}

const hostStyleSource = filesUnder(
  join(root, "apps/gents-desktop/src/styles"),
  (path) => path.endsWith(".css"),
)
  .map((path) => readFileSync(path, "utf8"))
  .join("\n");
const hostSelectors = [
  ...hostStyleSource.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/([^{}]+)\{/g),
].flatMap((match) => match[1].split(",").map((selector) => selector.trim()));
for (const className of [
  "primary-button",
  "ghost-button",
  "icon-button",
  "panel",
  "muted",
  "eyebrow",
  "field",
  "mono",
  "small",
  "chip",
  "chat-header",
  "chat-status",
  "composer-footer",
  "empty-transcript",
]) {
  const directSelector = new RegExp(
    `^\\.${className}(?::(?:[\\w-]+|not\\([^)]*\\)))*$`,
  );
  if (hostSelectors.some((selector) => directSelector.test(selector))) {
    failures.push(
      `host CSS must not directly redefine reusable package class .${className}`,
    );
  }
}

const fleetBaseStyles = readFileSync(
  join(root, "packages/gents-desktop-fleet/styles.css"),
  "utf8",
);
const fleetLayoutStyles = readFileSync(
  join(root, "packages/gents-desktop-fleet/styles/layout.css"),
  "utf8",
);
if (
  !fleetBaseStyles.includes("./styles/layout.css") ||
  !fleetLayoutStyles.includes(".fleet-inference-callout")
) {
  failures.push(
    "FleetDashboard inference callout styles must ship in the base fleet entrypoint",
  );
}

const mcpHealthStyles = readFileSync(
  join(root, "packages/gents-desktop-operations/styles/mcp-health.css"),
  "utf8",
);
if (!/@layer\s+mcp-health\s*\{/.test(mcpHealthStyles)) {
  failures.push("operations MCP health CSS must use the mcp-health layer");
}

const semanticPath = join(root, "packages/gents-desktop-tokens/semantic.css");
const semanticSource = readFileSync(semanticPath, "utf8");
const semanticTokens = new Set(
  [...semanticSource.matchAll(/(?:^|[{;\s])(--[\w-]+)\s*:/g)].map(
    (match) => match[1],
  ),
);
const hostTokensPath = join(root, "apps/gents-desktop/src/styles/tokens.css");
const hostTokens = readFileSync(hostTokensPath, "utf8");
for (const match of hostTokens.matchAll(
  /(?:^|[{;\s])(--(?:font|color|border|radius|space|text|motion)-[\w-]+|--(?:overlay|scrim)-rgb)\s*:/g,
)) {
  if (!semanticTokens.has(match[1])) {
    failures.push(
      `${relative(root, hostTokensPath)} overrides ${match[1]}, which is absent from the semantic contract`,
    );
  }
}

const semanticAccent = semanticSource.match(/--color-accent:\s*([^;]+);/)?.[1];
const fixtureStylesPath = join(root, "apps/fixture-host/src/styles.css");
const fixtureStyles = readFileSync(fixtureStylesPath, "utf8");
const fixtureAccent = fixtureStyles.match(/--color-accent:\s*([^;]+);/)?.[1];
if (
  !fixtureAccent ||
  fixtureAccent === semanticAccent ||
  fixtureAccent.toLowerCase() === "#06b250"
) {
  failures.push(
    `${relative(root, fixtureStylesPath)} must remap --color-accent to a distinct non-Source brand`,
  );
}
const fixtureApp = readFileSync(
  join(root, "apps/fixture-host/src/App.tsx"),
  "utf8",
);
if (!fixtureApp.includes('data-testid="fixture-brand"')) {
  failures.push(
    "fixture-host must pass a distinct brand node to FleetDashboard",
  );
}

const desktopAppCssPath = join(root, "apps/gents-desktop/src/App.css");
const desktopAppCss = readFileSync(desktopAppCssPath, "utf8");
const semanticImport = desktopAppCss.indexOf(
  '@import "@source-inc/gents-desktop-tokens/semantic.css";',
);
const hostTokenImport = desktopAppCss.indexOf('@import "./styles/tokens.css";');
if (
  semanticImport === -1 ||
  hostTokenImport === -1 ||
  semanticImport > hostTokenImport
) {
  failures.push(
    `${relative(root, desktopAppCssPath)} must import semantic.css before host token overrides`,
  );
}

const breakpointSource = readFileSync(
  join(root, "packages/gents-desktop-client/src/index.ts"),
  "utf8",
);
const narrowBreakpoint = Number(
  breakpointSource.match(/NARROW_BREAKPOINT_PX\s*=\s*(\d+)/)?.[1],
);
const tokenBreakpoint = Number(
  semanticSource.match(/--gents-narrow-breakpoint:\s*(\d+)px/)?.[1],
);
if (
  !Number.isInteger(narrowBreakpoint) ||
  tokenBreakpoint !== narrowBreakpoint
) {
  failures.push(
    `semantic breakpoint ${tokenBreakpoint || "<missing>"} must equal NARROW_BREAKPOINT_PX ${narrowBreakpoint || "<missing>"}`,
  );
}
for (const path of [
  "packages/gents-desktop-chat/styles/chat.css",
  "packages/gents-desktop-fleet/styles/layout.css",
  "packages/gents-desktop-operations/styles/rail.css",
]) {
  const css = readFileSync(join(root, path), "utf8");
  const values = [...css.matchAll(/@media\s*\(max-width:\s*(\d+)px\)/g)].map(
    (match) => Number(match[1]),
  );
  if (!values.includes(narrowBreakpoint)) {
    failures.push(
      `${path} must use the shared narrow breakpoint ${narrowBreakpoint}px`,
    );
  }
}

for (const [path, maximumLines] of [
  ["packages/gents-desktop-client/src/api.ts", 100],
  ["packages/gents-desktop-client/src/api/adapter.ts", 350],
  ["packages/gents-desktop-fleet/src/InferenceSetupWizard.tsx", 450],
  ["packages/gents-desktop-fleet/src/inference/steps.tsx", 400],
  ["packages/gents-desktop-chat/src/components/Transcript.tsx", 100],
  ["packages/gents-desktop-chat/src/components/codeTools/codeTools.ts", 100],
]) {
  const lines = readFileSync(join(root, path), "utf8").split(/\r?\n/).length;
  if (lines > maximumLines) {
    failures.push(`${path} has ${lines} lines; maximum is ${maximumLines}`);
  }
}

if (failures.length > 0) {
  console.error(failures.map((failure) => `- ${failure}`).join("\n"));
  process.exit(1);
}

console.log(
  `Desktop package boundaries and lockstep version ${workspaceVersion} are valid.`,
);
