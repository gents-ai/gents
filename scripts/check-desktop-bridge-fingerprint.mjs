import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

function capture(source, pattern, label) {
  const value = source.match(pattern)?.[1];
  if (!value) throw new Error(`could not read ${label}`);
  return value;
}

function tomlTable(source, name) {
  const lines = source.split(/\r?\n/);
  let body;
  for (const line of lines) {
    const heading = line.match(/^\s*(?:\[\[([^\]\r\n]+)\]\]|\[([^\]\r\n]+)\])\s*(?:#.*)?$/);
    if (heading) {
      if (body) break;
      if ((heading[1] ?? heading[2]) === name) body = [];
    } else if (body) {
      body.push(line);
    }
  }
  if (!body) throw new Error(`could not read [${name}]`);
  return body.join("\n");
}

export function bridgePackageVersion(workspaceCargo, cargo) {
  const packageTable = tomlTable(cargo, "package");
  if (/^\s*version\.workspace\s*=\s*true\s*(?:#.*)?$/m.test(packageTable)) {
    return capture(
      tomlTable(workspaceCargo, "workspace.package"),
      /^\s*version\s*=\s*"([^"]+)"\s*(?:#.*)?$/m,
      "workspace package version",
    );
  }
  return capture(packageTable, /^\s*version\s*=\s*"([^"]+)"\s*(?:#.*)?$/m, "bridge package version");
}

export function compareBridgeFingerprint(expected, actual) {
  for (const key of Object.keys(expected)) {
    if (expected[key] !== actual[key]) {
      throw new Error(
        `desktop bridge fingerprint ${key} drifted: Rust=${expected[key]} generated=${actual[key]}. ` +
          "Regenerate with: cargo test -p gents-desktop-bridge write_bindings -- --ignored",
      );
    }
  }
}

export async function checkDesktopBridgeFingerprint(root) {
  const rust = await readFile(path.join(root, "crates/gents-desktop-bridge/src/contract.rs"), "utf8");
  const generated = await readFile(path.join(root, "packages/gents-desktop-client/src/generated/BridgeContractFingerprint.ts"), "utf8");
  const cargo = await readFile(path.join(root, "crates/gents-desktop-bridge/Cargo.toml"), "utf8");
  const workspaceCargo = await readFile(path.join(root, "Cargo.toml"), "utf8");
  const expected = {
    version: capture(rust, /CONTRACT_VERSION:\s*&str\s*=\s*"([^"]+)"/, "Rust contract version"),
    hash: capture(rust, /WIRE_SCHEMA_HASH:\s*&str\s*=\s*\n?\s*"([^"]+)"/, "Rust wire hash"),
    package: bridgePackageVersion(workspaceCargo, cargo),
  };
  const actual = {
    version: capture(generated, /BRIDGE_CONTRACT_VERSION\s*=\s*"([^"]+)"/, "generated contract version"),
    hash: capture(generated, /BRIDGE_WIRE_SCHEMA_HASH\s*=\s*"([^"]+)"/, "generated wire hash"),
    package: capture(generated, /BRIDGE_PACKAGE_VERSION\s*=\s*"([^"]+)"/, "generated package version"),
  };
  compareBridgeFingerprint(expected, actual);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  await checkDesktopBridgeFingerprint(root);
}
