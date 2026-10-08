import { describe, expect, it, vi } from "vitest";
import type { ManagedServerStatus } from "@source-inc/gents-desktop-client";

import { createClientStore } from "../src/hooks/clientStore";
import { createLocalServer } from "../src/hooks/localServer";
import { createLocalServerStore } from "../src/hooks/localServerStore";

const status = (state: ManagedServerStatus["state"]) =>
  ({
    state,
    autoStart: true,
    agentName: "Workshop Agent",
    agentDid: null,
    graphql: null,
    effectiveToolCeiling: "readwrite",
    effectiveToolRoot: "/Users/test",
    suggestedToolRoot: "/Users/test",
    pairingReady: state === "running",
    error: null,
  }) as unknown as ManagedServerStatus;

/* a promise the test settles */
function later<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}

function owner(api: object) {
  const store = createLocalServerStore();
  const server = createLocalServer({
    api: api as never,
    store,
    client: createClientStore(),
  });
  return { server, store };
}

describe("the local server's owner", () => {
  it("joins a start asked for while the same start runs", async () => {
    const started = later<ManagedServerStatus>();
    const startManagedServer = vi.fn(() => started.promise);
    const { server, store } = owner({ startManagedServer });

    const first = server.startLocalServer("Workshop Agent");
    const second = server.startLocalServer("Workshop Agent");
    await Promise.resolve();
    started.resolve(status("running"));

    await expect(first).resolves.toMatchObject({ state: "running" });
    await expect(second).resolves.toMatchObject({ state: "running" });
    expect(startManagedServer).toHaveBeenCalledOnce();
    expect(store.getState().status?.state).toBe("running");
  });

  it("runs a stop asked for during a start after it, not over it", async () => {
    const started = later<ManagedServerStatus>();
    const order: string[] = [];
    const { server, store } = owner({
      startManagedServer: vi.fn(() => {
        order.push("start");
        return started.promise;
      }),
      stopManagedServer: vi.fn(async () => {
        order.push("stop");
        return status("stopped");
      }),
    });

    const start = server.startLocalServer("Workshop Agent");
    const stop = server.stopLocalServer();
    await vi.waitFor(() => expect(order).toEqual(["start"]));
    expect(store.getState().operation).toBe("start");
    started.resolve(status("running"));

    await start;
    await stop;
    expect(order).toEqual(["start", "stop"]);
    expect(store.getState()).toMatchObject({
      status: { state: "stopped" },
      operation: null,
    });
  });

  it("keeps an operation's result over a read begun before it", async () => {
    const read = later<ManagedServerStatus>();
    const { server, store } = owner({
      managedServerStatus: vi.fn(() => read.promise),
      stopManagedServer: vi.fn(async () => status("stopped")),
    });

    const stale = server.refreshLocalServer();
    await server.stopLocalServer();
    read.resolve(status("running"));
    await stale;

    expect(store.getState().status?.state).toBe("stopped");
  });

  it("says why a read failed and clears it once one succeeds", async () => {
    const managedServerStatus = vi
      .fn()
      .mockRejectedValueOnce(new Error("service unreachable"))
      .mockResolvedValueOnce(status("running"));
    const { server, store } = owner({ managedServerStatus });

    await expect(server.refreshLocalServer()).resolves.toBeNull();
    expect(store.getState()).toMatchObject({
      status: null,
      readFailure: "Error: service unreachable",
    });
    await server.refreshLocalServer();
    expect(store.getState()).toMatchObject({
      status: { state: "running" },
      readFailure: null,
    });
  });
});
