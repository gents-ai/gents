import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  assertCompatibleBridgeContract,
  MINIMUM_BRIDGE_CONTRACT_VERSION,
  PACKAGE_VERSION,
  type DesktopBridgeContract,
} from "./client.js";

function versionParts(version: string): [number, number] {
  const match = /^(\d+)\.(\d+)$/.exec(version);
  if (!match) throw new Error(`invalid test version ${version}`);
  return [Number(match[1]), Number(match[2])];
}

function contract(
  contractVersion: string,
  packageVersion = PACKAGE_VERSION,
): DesktopBridgeContract {
  return {
    contractVersion,
    packageVersion,
    events: [],
    eventReasons: [],
    errorCodes: [],
    commands: [],
    permissionSets: [],
  };
}

describe("desktop bridge compatibility", () => {
  it("accepts the supported additive range and rejects incompatible versions", () => {
    const [major, minor] = versionParts(MINIMUM_BRIDGE_CONTRACT_VERSION);

    expect(() =>
      assertCompatibleBridgeContract(contract(`${major}.${minor}`)),
    ).not.toThrow();
    expect(() =>
      assertCompatibleBridgeContract(contract(`${major}.${minor + 1}`)),
    ).not.toThrow();

    for (const version of [
      "1.6",
      `${major}.${minor - 1}`,
      `${major - 1}.${minor}`,
      `${major + 1}.0`,
      `${major}`,
      `${major}.${minor}.0`,
      ` ${major}.${minor}`,
      "NaN.6",
    ]) {
      expect(() => assertCompatibleBridgeContract(contract(version))).toThrow(
        "Incompatible Gents desktop bridge contract",
      );
    }
  });

  it("keeps package release identity exact", () => {
    expect(() =>
      assertCompatibleBridgeContract(
        contract(MINIMUM_BRIDGE_CONTRACT_VERSION, "0.13.0"),
      ),
    ).toThrow("Gents desktop package mismatch");
  });

  it("accepts the actual packaged Rust bridge fingerprint", () => {
    const fingerprint = JSON.parse(
      readFileSync(
        new URL("../../../contracts/desktop-bridge.json", import.meta.url),
        "utf8",
      ),
    ) as DesktopBridgeContract;

    expect(() => assertCompatibleBridgeContract(fingerprint)).not.toThrow();
  });
});
