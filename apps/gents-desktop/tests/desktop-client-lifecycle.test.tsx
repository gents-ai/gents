import { waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createSelectionStore } from "../src/hooks/selectionStore";
import { readSession, writeSession } from "../src/hooks/sessionStore";
import { lifecycleFor } from "./shell-fixture";

const ownership = vi.hoisted(() => ({ main: true }));
vi.mock("../src/lib/shellPlatform", () => ({
  isMobileTauriShell: () => false,
  ownsAutomaticRecovery: () => ownership.main,
}));
beforeEach(() => {
  ownership.main = true;
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, reject, resolve };
}

describe("desktop client restart selection ordering", () => {
  it.each([true, false])(
    "only the original view observes the managed runtime without starting it (owner=%s)",
    async (main) => {
      ownership.main = main;
      const snapshot = {
        bootstrap: { clientStateExists: true, savedPeers: [] },
        client: {},
      };
      const api = {
        managedServerStatus: vi
          .fn()
          .mockResolvedValue({ state: "stopped", autoStart: true, agentName: "local" }),
        startManagedServer: vi.fn().mockResolvedValue({ state: "running" }),
        fetchDesktopSnapshot: vi.fn().mockResolvedValue(snapshot),
      };
      const lifecycle = lifecycleFor(api, { supportsManagedServer: true });
      await waitFor(() => expect(lifecycle.startupPhase).toBe("ready"));
      expect(api.startManagedServer).not.toHaveBeenCalled();
      expect(api.managedServerStatus).toHaveBeenCalledTimes(main ? 1 : 0);
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledOnce();
    },
  );
  it("does not let an older startup refresh replace a newer client-start snapshot", async () => {
    const refresh = deferred<Record<string, unknown>>();
    const start = deferred<Record<string, unknown>>();
    const newer = {
      bootstrap: { clientStateExists: true, savedPeers: [] },
      client: {},
    };
    const store = createSelectionStore();
    const api = {
      fetchDesktopSnapshot: vi
        .fn()
        .mockReturnValueOnce(refresh.promise)
        .mockResolvedValue(newer),
      startDesktopClient: vi.fn(() => start.promise),
    };
    const lifecycle = lifecycleFor(api, { selection: store });
    await waitFor(() => expect(api.fetchDesktopSnapshot).toHaveBeenCalledOnce());

    const starting = lifecycle.ensureDesktopClientStarted();
    start.resolve(newer);
    await starting;
    expect(lifecycle.snapshot).toEqual(newer);
    expect(lifecycle.startupPhase).toBe("ready");

    refresh.resolve({
      bootstrap: { clientStateExists: false, savedPeers: [] },
      client: null,
    });
    await refresh.promise;
    expect(lifecycle.snapshot).toEqual(newer);
    expect(lifecycle.startupPhase).toBe("ready");
  });

  it.each(["start", "restart"])(
    "observes successful %s after an intervening stopped refresh",
    async (operation) => {
      /* the start or restart asked for, alone: a view that does not own
         automatic recovery does not start the client by itself */
      ownership.main = false;
      const start = deferred<Record<string, unknown>>();
      const stopped = {
        bootstrap: { clientStateExists: true, savedPeers: [{}] },
        client: null,
      };
      const ready = { ...stopped, client: {} };
      const api = {
        fetchDesktopSnapshot: vi.fn().mockResolvedValue(stopped),
        shutdownDesktopClient: vi.fn(async () => stopped),
        startDesktopClient: vi.fn(() => start.promise),
      };
      const lifecycle = lifecycleFor(api);
      await waitFor(() => expect(lifecycle.snapshot).not.toBeNull());
      const pending =
        operation === "start"
          ? lifecycle.ensureDesktopClientStarted()
          : lifecycle.restartDesktopClient("test");
      await waitFor(() => expect(api.startDesktopClient).toHaveBeenCalledOnce());
      await lifecycle.refreshSnapshot();
      expect(lifecycle.snapshot).toEqual(stopped);
      api.fetchDesktopSnapshot.mockResolvedValue(ready);
      start.resolve(ready);
      await pending;
      expect(lifecycle.snapshot).toEqual(ready);
      expect(lifecycle.startupPhase).toBe("ready");
      expect(lifecycle.starting).toBe(false);
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(3);
    },
  );

  it("does not let an older failed start replace a newer refresh success", async () => {
    const start = deferred<Record<string, unknown>>();
    const initial = {
      bootstrap: { clientStateExists: true, savedPeers: [{}] },
      client: null,
    };
    const refreshed = {
      bootstrap: { clientStateExists: true, savedPeers: [] },
      client: {},
    };
    const api = {
      fetchDesktopSnapshot: vi
        .fn()
        .mockResolvedValueOnce(initial)
        .mockResolvedValueOnce(refreshed),
      startDesktopClient: vi.fn(() => start.promise),
    };
    const lifecycle = lifecycleFor(api);
    await waitFor(() => expect(lifecycle.snapshot).not.toBeNull());

    const starting = lifecycle.ensureDesktopClientStarted();
    await lifecycle.refreshSnapshot();
    start.reject(new Error("stale start failed"));
    await expect(starting).rejects.toThrow("stale start failed");

    expect(lifecycle.snapshot).toEqual(refreshed);
    expect(lifecycle.startupPhase).toBe("ready");
    expect(lifecycle.errors).not.toContain("Error: stale start failed");
  });

  it("reports a failed start when a newer read only observed the stopped client", async () => {
    const start = deferred<Record<string, unknown>>();
    const stopped = {
      bootstrap: { clientStateExists: true, savedPeers: [{}] },
      client: null,
    };
    const api = {
      fetchDesktopSnapshot: vi.fn().mockResolvedValue(stopped),
      startDesktopClient: vi.fn(() => start.promise),
    };
    const lifecycle = lifecycleFor(api);
    await waitFor(() => expect(lifecycle.snapshot).not.toBeNull());
    const pending = lifecycle.ensureDesktopClientStarted();
    await lifecycle.refreshSnapshot();
    start.reject(new Error("start failed"));
    await expect(pending).rejects.toThrow("start failed");
    expect(lifecycle.startupPhase).toBe("client-error");
    expect(lifecycle.starting).toBe(false);
    expect(lifecycle.errors).toContain("Error: start failed");
  });

  it("does not clear a session selected while a restart from new-compose is pending", async () => {
    const restarted = deferred<Record<string, unknown>>();
    const store = createSelectionStore();
    const api = {
      fetchDesktopSnapshot: vi.fn(async () => ({
        bootstrap: { clientStateExists: false, savedPeers: [] },
        client: null,
      })),
      shutdownDesktopClient: vi.fn(async () => undefined),
      startDesktopClient: vi.fn(() => restarted.promise),
    };
    const lifecycle = lifecycleFor(api, { selection: store });
    await waitFor(() => expect(api.fetchDesktopSnapshot).toHaveBeenCalled());

    const restart = lifecycle.restartDesktopClient("test");
    await waitFor(() => expect(api.startDesktopClient).toHaveBeenCalled());
    store.setState({ sessionId: "newly-selected-session" });
    writeSession(lifecycle.stores.session, {
      sessionId: "newly-selected-session",
    } as never);
    restarted.resolve({
      bootstrap: { clientStateExists: true, savedPeers: [] },
      client: null,
    });
    await restart;

    expect(readSession(lifecycle.stores.session)?.sessionId).toBe(
      "newly-selected-session",
    );
  });

  it("recovers client-error when a successful start is finally observed after a failed read", async () => {
    const stopped = {
      bootstrap: { clientStateExists: true, savedPeers: [{}] },
      client: null,
    };
    const ready = { ...stopped, client: {} };
    const api = {
      fetchDesktopSnapshot: vi
        .fn()
        .mockResolvedValueOnce(stopped)
        .mockRejectedValueOnce(new Error("transient IPC read failure"))
        .mockResolvedValue(ready),
      startDesktopClient: vi.fn().mockResolvedValue(ready),
    };
    /* the stopped client starts by itself; its start succeeds, and the read
       after it fails */
    const lifecycle = lifecycleFor(api);
    await waitFor(() => expect(lifecycle.startupPhase).toBe("client-error"));
    expect(api.startDesktopClient).toHaveBeenCalledOnce();
    await lifecycle.refreshSnapshot();
    expect(lifecycle.startupPhase).toBe("ready");
    expect(lifecycle.snapshot).toEqual(ready);
  });
});
