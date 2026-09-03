import { describe, expect, it } from "vitest";

import {
  createDesktopClient,
  EXPECTED_BRIDGE_WIRE_SCHEMA_HASH,
  BRIDGE_CONTRACT_VERSION,
  PACKAGE_VERSION,
} from "./client.js";
import { createDesktopStore } from "./store.js";
import { createMemoryTransport } from "./testing.js";

function wait(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function compatibleBridgeContract() {
  return {
    contractVersion: BRIDGE_CONTRACT_VERSION,
    packageVersion: PACKAGE_VERSION,
    wireSchemaHash: EXPECTED_BRIDGE_WIRE_SCHEMA_HASH,
    events: [],
    eventReasons: [],
    errorCodes: [],
    commands: [],
    permissionSets: [],
  };
}

describe("createDesktopStore", () => {
  it("routes hydration retry through its explicit mutation command", async () => {
    const transport = createMemoryTransport({
      handlers: {
        desktop_session_hydration_retry: () => undefined,
      },
    });
    const client = createDesktopClient(transport);

    await client.api.retrySessionHydration("session-1");

    expect(transport.calls).toEqual([
      {
        command: "desktop_session_hydration_retry",
        args: { sessionId: "session-1", agentDid: null },
      },
    ]);
  });

  it("binds the full domain API to the injected transport", async () => {
    const transport = createMemoryTransport({
      handlers: {
        desktop_network_status: () => ({ p2pEnabled: true }),
      },
    });
    const client = createDesktopClient(transport);

    await expect(client.api.fetchNetworkStatus()).resolves.toEqual({
      p2pEnabled: true,
    });
    expect(transport.calls).toEqual([
      { command: "desktop_network_status", args: undefined },
    ]);
  });

  it("serializes concurrent starts and owns one update subscription", async () => {
    let starts = 0;
    const transport = createMemoryTransport({
      handlers: {
        desktop_client_start: async () => {
          starts += 1;
          await wait(5);
          return {};
        },
        desktop_bridge_contract: compatibleBridgeContract,
        desktop_client_snapshot: () => ({ bootstrap: {}, client: null }),
        desktop_client_shutdown: () => ({}),
      },
    });
    const store = createDesktopStore(createDesktopClient(transport), {
      refreshDebounceMs: 1,
    });

    await Promise.all([store.start(), store.start(), store.start()]);

    expect(starts).toBe(1);
    expect(transport.listenerCount()).toBe(1);
    await store.stop();
    expect(transport.listenerCount()).toBe(0);
  });

  it("coalesces a burst of update events into one refresh", async () => {
    let snapshots = 0;
    const transport = createMemoryTransport({
      handlers: {
        desktop_client_start: () => ({}),
        desktop_bridge_contract: compatibleBridgeContract,
        desktop_client_snapshot: () => ({
          bootstrap: {},
          client: null,
          observation: ++snapshots,
        }),
        desktop_client_shutdown: () => ({}),
      },
    });
    const store = createDesktopStore(createDesktopClient(transport), {
      refreshDebounceMs: 5,
    });
    await store.start();
    const baseline = snapshots;

    for (let index = 0; index < 10; index += 1) {
      transport.emitClientUpdated({ reason: `event-${index}` });
    }
    await wait(20);

    expect(snapshots - baseline).toBe(1);
    await store.stop();
  });

  it("serializes direct refreshes so an older response cannot win", async () => {
    let snapshots = 0;
    const transport = createMemoryTransport({
      handlers: {
        desktop_client_snapshot: async () => {
          const observation = ++snapshots;
          if (observation === 1) {
            await wait(10);
          }
          return { bootstrap: {}, client: null, observation };
        },
      },
    });
    const store = createDesktopStore(createDesktopClient(transport), {
      refreshDebounceMs: 1,
    });

    await Promise.all([store.refresh(), store.refresh()]);

    expect(snapshots).toBe(2);
    expect(store.getState().snapshot).toMatchObject({ observation: 2 });
  });
});
