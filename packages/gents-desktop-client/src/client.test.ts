import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  assertExactBridgeContract,
  createDesktopClient,
  EXPECTED_BRIDGE_WIRE_SCHEMA_HASH,
  BRIDGE_CONTRACT_VERSION,
  PACKAGE_VERSION,
  type DesktopBridgeContract,
} from "./client.js";
import { createMemoryTransport } from "./testing.js";

function contract(
  contractVersion: string,
  packageVersion = PACKAGE_VERSION,
): DesktopBridgeContract {
  return {
    contractVersion,
    packageVersion,
    wireSchemaHash: EXPECTED_BRIDGE_WIRE_SCHEMA_HASH,
    events: [],
    eventReasons: [],
    errorCodes: [],
    commands: [],
    permissionSets: [],
  };
}

describe("desktop bridge contract", () => {
  it("requires the exact contract version", () => {
    expect(() =>
      assertExactBridgeContract(contract(BRIDGE_CONTRACT_VERSION)),
    ).not.toThrow();

    for (const version of [
      "1.6",
      "4.2",
      "5.1",
      "6.1",
      "5",
      "5.0.0",
      " 5.0",
      "NaN.6",
    ]) {
      expect(() => assertExactBridgeContract(contract(version))).toThrow(
        "Incompatible Gents desktop bridge contract",
      );
    }
  });

  it("keeps package release identity exact", () => {
    expect(() =>
      assertExactBridgeContract(
        contract(BRIDGE_CONTRACT_VERSION, "0.13.0"),
      ),
    ).toThrow("Gents desktop package mismatch");
  });

  it("rejects a mismatched generated wire schema", () => {
    expect(() =>
      assertExactBridgeContract({
        ...contract(BRIDGE_CONTRACT_VERSION),
        wireSchemaHash: "stale-wire-schema",
      }),
    ).toThrow("Incompatible Gents desktop wire schema");
  });

  it("accepts the actual packaged Rust bridge fingerprint", () => {
    const fingerprint = JSON.parse(
      readFileSync(
        new URL("../../../contracts/desktop-bridge.json", import.meta.url),
        "utf8",
      ),
    ) as DesktopBridgeContract;

    expect(() => assertExactBridgeContract(fingerprint)).not.toThrow();
  });

  it("rejects an old bridge on the default app API before starting", async () => {
    let starts = 0;
    const transport = createMemoryTransport({
      handlers: {
        desktop_bridge_contract: () => contract("2.0"),
        desktop_client_start: () => {
          starts += 1;
          return {};
        },
      },
    });

    await expect(
      createDesktopClient(transport).api.startDesktopClient(),
    ).rejects.toThrow("Incompatible Gents desktop bridge contract 2.0");
    expect(starts).toBe(0);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_bridge_contract",
    ]);
  });

  it("routes status enrollment through the authenticated bridge command", async () => {
    const enrollment = {
      requestId: "request-1",
      networkId: "network-1",
      adminDid: "did:key:zAdmin",
      serverPeer: "server-peer-1",
      ownerAgent: "did:key:zAgent",
      state: "pending_approval",
    };
    const transport = createMemoryTransport({
      handlers: {
        desktop_peer_enroll_status: (args) => {
          expect(args).toEqual({
            request: { serverAddress: "http://amy.local:9191" },
          });
          return enrollment;
        },
      },
    });

    await expect(
      createDesktopClient(transport).api.requestStatusEnrollment(
        "http://amy.local:9191",
      ),
    ).resolves.toEqual(enrollment);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_peer_enroll_status",
    ]);
  });
});
