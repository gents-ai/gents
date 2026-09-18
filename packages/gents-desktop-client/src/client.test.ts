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
  it("carries each view's explicit agent through task, schedule and retry actions", async () => {
    const transport = createMemoryTransport({
      handlers: {
        desktop_task_run: () => ({}),
        desktop_schedule_run: () => ({}),
        desktop_request_retry: () => ({}),
        desktop_request_resend: () => ({}),
      },
    });
    const { api } = createDesktopClient(transport);
    for (const agentDid of ["did:alpha", "did:beta"]) {
      await api.runTask({ taskId: "daily", agentDid });
      await api.runSchedule({ scheduleId: "daily", agentDid });
      await api.retryRequest("same-request-id", agentDid);
      await api.resendRequest("same-request-id", agentDid);
    }
    expect(transport.calls).toEqual(
      ["did:alpha", "did:beta"].flatMap((agentDid) => [
        {
          command: "desktop_task_run",
          args: { request: { taskId: "daily", agentDid } },
        },
        {
          command: "desktop_schedule_run",
          args: { request: { scheduleId: "daily", agentDid } },
        },
        {
          command: "desktop_request_retry",
          args: { requestId: "same-request-id", agentDid },
        },
        {
          command: "desktop_request_resend",
          args: { requestId: "same-request-id", agentDid },
        },
      ]),
    );
  });
  it("requires the exact contract version", () => {
    expect(() =>
      assertExactBridgeContract(contract(BRIDGE_CONTRACT_VERSION)),
    ).not.toThrow();

    for (const version of [
      "1.6",
      "4.2",
      "5.1",
      "6.0",
      "7.1",
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
      assertExactBridgeContract(contract(BRIDGE_CONTRACT_VERSION, "0.13.0")),
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

  it("invokes managed runtime restart with the exact reviewed authority", async () => {
    const restarted = {
      state: "running",
      autoStart: true,
      agentName: "Workshop Agent",
      agentDid: "did:key:agent",
      graphql: "http://127.0.0.1:9191/graphql",
      effectiveToolCeiling: "meta-only",
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      error: null,
    };
    const transport = createMemoryTransport({
      handlers: {
        desktop_managed_server_restart: (args) => {
          expect(args).toEqual({
            request: {
              agentName: "Workshop Agent",
              toolCeiling: "meta-only",
              toolRoot: null,
            },
          });
          return restarted;
        },
      },
    });

    await expect(
      createDesktopClient(transport).api.restartManagedServer(
        "Workshop Agent",
        {
          toolCeiling: "meta-only",
          toolRoot: null,
        },
      ),
    ).resolves.toEqual(restarted);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_managed_server_restart",
    ]);
  });
});
