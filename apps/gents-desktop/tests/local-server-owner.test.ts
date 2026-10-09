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
  it("keeps different start authorities in order", async () => {
    const firstStart = later<ManagedServerStatus>();
    const startManagedServer = vi
      .fn()
      .mockImplementationOnce(() => firstStart.promise)
      .mockResolvedValue(status("running"));
    const { server } = owner({ startManagedServer });
    const first = server.startLocalServer("Workshop Agent", {
      toolCeiling: "readonly",
      toolRoot: "/one",
    });
    const second = server.startLocalServer("Workshop Agent", {
      toolCeiling: "readwrite",
      toolRoot: "/two",
    });
    await vi.waitFor(() => expect(startManagedServer).toHaveBeenCalledTimes(1));
    firstStart.resolve(status("running"));
    await Promise.all([first, second]);
    expect(startManagedServer).toHaveBeenNthCalledWith(2, "Workshop Agent", {
      toolCeiling: "readwrite",
      toolRoot: "/two",
    });
  });

  it("does not swallow an opposing autostart setting or the setup commit", async () => {
    const enabled = later<ManagedServerStatus>();
    const setManagedServerAutoStart = vi
      .fn()
      .mockImplementationOnce(() => enabled.promise)
      .mockResolvedValue({ ...status("stopped"), autoStart: false });
    const commitManagedServerAutoStart = vi.fn().mockResolvedValue(status("stopped"));
    const { server } = owner({
      setManagedServerAutoStart,
      commitManagedServerAutoStart,
    });
    const first = server.setLocalServerAutoStart(true);
    const second = server.setLocalServerAutoStart(false);
    const third = server.commitLocalServerAutoStart("Workshop Agent");
    await vi.waitFor(() => expect(setManagedServerAutoStart).toHaveBeenCalledTimes(1));
    enabled.resolve(status("stopped"));
    await Promise.all([first, second, third]);
    expect(setManagedServerAutoStart.mock.calls).toEqual([[true], [false]]);
    expect(commitManagedServerAutoStart).toHaveBeenCalledWith("Workshop Agent");
  });

  it("queues a stop until setup readiness finishes starting", async () => {
    const started = later<ManagedServerStatus>();
    let state = status("stopped");
    const startManagedServer = vi.fn(async () => {
      state = await started.promise;
      return state;
    });
    const stopManagedServer = vi.fn(async () => status("stopped"));
    const { server, store } = owner({
      managedServerStatus: vi.fn(async () => state),
      startManagedServer,
      stopManagedServer,
    });
    const ready = server.ensureLocalServerServing("Workshop Agent");
    await vi.waitFor(() => expect(startManagedServer).toHaveBeenCalledOnce());
    expect(store.getState().operation).toBe("ensure");
    const stop = server.stopLocalServer();
    expect(stopManagedServer).not.toHaveBeenCalled();
    started.resolve(status("running"));
    await Promise.all([ready, stop]);
    expect(stopManagedServer).toHaveBeenCalledOnce();
    expect(store.getState().status?.state).toBe("stopped");
  });

  it("queues setup readiness behind a pending stop", async () => {
    const stopped = later<ManagedServerStatus>();
    const managedServerStatus = vi.fn().mockResolvedValue(status("running"));
    const stopManagedServer = vi.fn(() => stopped.promise);
    const { server } = owner({ managedServerStatus, stopManagedServer });
    const stop = server.stopLocalServer();
    await vi.waitFor(() => expect(stopManagedServer).toHaveBeenCalledOnce());
    const ready = server.ensureLocalServerServing("Workshop Agent");
    expect(managedServerStatus).not.toHaveBeenCalled();
    stopped.resolve(status("stopped"));
    await Promise.all([stop, ready]);
    expect(managedServerStatus).toHaveBeenCalled();
  });

  it("queues a stop behind the post-approval startup restore", async () => {
    vi.useFakeTimers();
    try {
      const started = later<ManagedServerStatus>();
      const managedServerStatus = vi
        .fn()
        .mockResolvedValueOnce({ ...status("disabled"), approvalRequired: true })
        .mockResolvedValue(status("stopped"));
      const startManagedServer = vi.fn(() => started.promise);
      const stopManagedServer = vi.fn().mockResolvedValue(status("stopped"));
      const { server, store } = owner({
        managedServerStatus,
        startManagedServer,
        stopManagedServer,
      });
      const restore = server.restoreLocalServer();
      await vi.advanceTimersByTimeAsync(1_000);
      expect(startManagedServer).toHaveBeenCalledOnce();
      expect(store.getState().operation).toBe("restore");
      const stop = server.stopLocalServer();
      expect(stopManagedServer).not.toHaveBeenCalled();
      started.resolve(status("running"));
      await Promise.all([restore, stop]);
      expect(stopManagedServer).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not publish an old pairing read after a stop", async () => {
    const read = later<ManagedServerStatus>();
    const { server, store } = owner({
      managedServerStatus: vi.fn(() => read.promise),
      stopManagedServer: vi.fn().mockResolvedValue(status("stopped")),
    });
    const pairing = server.awaitLocalServerPairing();
    await server.stopLocalServer();
    read.resolve(status("running"));
    await pairing;
    expect(store.getState().status?.state).toBe("stopped");
  });
  it("does not publish an old settling read after a stop", async () => {
    vi.useFakeTimers();
    try {
      const read = later<ManagedServerStatus>();
      const { server, store } = owner({
        managedServerStatus: vi.fn(() => read.promise),
        stopManagedServer: vi.fn().mockResolvedValue(status("stopped")),
      });
      const settling = server.settleLocalServer(status("starting"));
      await vi.advanceTimersByTimeAsync(1_000);
      await server.stopLocalServer();
      read.resolve(status("running"));
      await settling;
      expect(store.getState().status?.state).toBe("stopped");
    } finally {
      vi.useRealTimers();
    }
  });
  it("does not let an old settling read clear a newer operation's wait", async () => {
    vi.useFakeTimers();
    try {
      const oldRead = later<ManagedServerStatus>();
      const started = later<ManagedServerStatus>();
      const managedServerStatus = vi
        .fn()
        .mockImplementationOnce(() => oldRead.promise)
        .mockResolvedValue({ ...status("starting"), runtimeBooting: true });
      const { server, store } = owner({
        managedServerStatus,
        startManagedServer: vi.fn(() => started.promise),
      });
      const settling = server.settleLocalServer(status("starting"));
      await vi.advanceTimersByTimeAsync(1_000);
      const start = server.startLocalServer("Workshop Agent");
      await vi.advanceTimersByTimeAsync(1_000);
      expect(store.getState().wait?.kind).toBe("updating");
      oldRead.resolve(status("running"));
      await settling;
      expect(store.getState().wait?.kind).toBe("updating");
      started.resolve(status("running"));
      await start;
      expect(store.getState().wait).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});
