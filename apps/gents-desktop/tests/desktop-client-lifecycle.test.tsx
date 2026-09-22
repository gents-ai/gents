import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useDesktopClientLifecycle } from "../src/hooks/useDesktopClientLifecycle";

const ownership = vi.hoisted(() => ({ main: true }));
vi.mock("../src/lib/shellPlatform", () => ({
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
      const { result } = renderHook(() =>
        useDesktopClientLifecycle({
          api,
          supportsManagedServer: true,
          refreshSession: vi.fn(async () => null),
          selectedSessionIdRef: { current: null },
          setError: vi.fn(),
          setSession: vi.fn(),
        } as unknown as Parameters<typeof useDesktopClientLifecycle>[0]),
      );
      await waitFor(() => expect(result.current.startupPhase).toBe("ready"));
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
    const selectedSessionIdRef = { current: null as string | null };
    const api = {
      fetchDesktopSnapshot: vi
        .fn()
        .mockReturnValueOnce(refresh.promise)
        .mockResolvedValue(newer),
      startDesktopClient: vi.fn(() => start.promise),
    };
    const { result } = renderHook(() =>
      useDesktopClientLifecycle({
        api,
        supportsManagedServer: false,
        refreshSession: vi.fn(async () => null),
        selectedSessionIdRef,
        setError: vi.fn(),
        setSession: vi.fn(),
      } as unknown as Parameters<typeof useDesktopClientLifecycle>[0]),
    );
    await waitFor(() => expect(api.fetchDesktopSnapshot).toHaveBeenCalledOnce());

    let starting!: Promise<Record<string, unknown> | null>;
    act(() => {
      starting = result.current.ensureDesktopClientStarted();
    });
    start.resolve(newer);
    await act(async () => starting);
    expect(result.current.snapshot).toBe(newer);
    expect(result.current.startupPhase).toBe("ready");
    expect(result.current.loading).toBe(false);

    refresh.resolve({
      bootstrap: { clientStateExists: false, savedPeers: [] },
      client: null,
    });
    await act(async () => refresh.promise);
    expect(result.current.snapshot).toBe(newer);
    expect(result.current.startupPhase).toBe("ready");
    expect(result.current.loading).toBe(false);
  });

  it.each(["start", "restart"])(
    "observes successful %s after an intervening stopped refresh",
    async (operation) => {
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
      const { result } = renderHook(() =>
        useDesktopClientLifecycle({
          api,
          supportsManagedServer: false,
          refreshSession: vi.fn(async () => null),
          selectedSessionIdRef: { current: null },
          setError: vi.fn(),
          setSession: vi.fn(),
        } as unknown as Parameters<typeof useDesktopClientLifecycle>[0]),
      );
      await waitFor(() => expect(result.current.loading).toBe(false));
      let pending!: Promise<unknown>;
      act(() => {
        pending =
          operation === "start"
            ? result.current.ensureDesktopClientStarted()
            : result.current.restartDesktopClient("test");
      });
      await waitFor(() => expect(api.startDesktopClient).toHaveBeenCalledOnce());
      await act(async () => result.current.refreshSnapshot());
      expect(result.current.snapshot).toBe(stopped);
      api.fetchDesktopSnapshot.mockResolvedValue(ready);
      start.resolve(ready);
      await act(async () => pending);
      expect(result.current.snapshot).toBe(ready);
      expect(result.current.startupPhase).toBe("ready");
      expect(result.current.starting).toBe(false);
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
    const setError = vi.fn();
    const api = {
      fetchDesktopSnapshot: vi
        .fn()
        .mockResolvedValueOnce(initial)
        .mockResolvedValueOnce(refreshed),
      startDesktopClient: vi.fn(() => start.promise),
    };
    const { result } = renderHook(() =>
      useDesktopClientLifecycle({
        api,
        supportsManagedServer: false,
        refreshSession: vi.fn(async () => null),
        selectedSessionIdRef: { current: null },
        setError,
        setSession: vi.fn(),
      } as unknown as Parameters<typeof useDesktopClientLifecycle>[0]),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    let starting!: Promise<Record<string, unknown> | null>;
    act(() => {
      starting = result.current.ensureDesktopClientStarted();
    });
    await act(async () => result.current.refreshSnapshot());
    start.reject(new Error("stale start failed"));
    await act(async () => starting);

    expect(result.current.snapshot).toBe(refreshed);
    expect(result.current.startupPhase).toBe("ready");
    expect(setError).not.toHaveBeenCalledWith("Error: stale start failed");
  });

  it("reports a failed start when a newer read only observed the stopped client", async () => {
    const start = deferred<Record<string, unknown>>();
    const stopped = {
      bootstrap: { clientStateExists: true, savedPeers: [{}] },
      client: null,
    };
    const setError = vi.fn();
    const api = {
      fetchDesktopSnapshot: vi.fn().mockResolvedValue(stopped),
      startDesktopClient: vi.fn(() => start.promise),
    };
    const { result } = renderHook(() =>
      useDesktopClientLifecycle({
        api,
        supportsManagedServer: false,
        refreshSession: vi.fn(async () => null),
        selectedSessionIdRef: { current: null },
        setError,
        setSession: vi.fn(),
      } as unknown as Parameters<typeof useDesktopClientLifecycle>[0]),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    let pending!: Promise<unknown>;
    act(() => {
      pending = result.current.ensureDesktopClientStarted();
    });
    await act(async () => result.current.refreshSnapshot());
    start.reject(new Error("start failed"));
    await act(async () => pending);
    expect(result.current.startupPhase).toBe("client-error");
    expect(result.current.starting).toBe(false);
    expect(setError).toHaveBeenCalledWith("Error: start failed");
  });

  it("does not clear a session selected while a restart from new-compose is pending", async () => {
    const restarted = deferred<Record<string, unknown>>();
    const selectedSessionIdRef = { current: null as string | null };
    const setSession = vi.fn();
    const api = {
      fetchDesktopSnapshot: vi.fn(async () => ({
        bootstrap: { clientStateExists: false, savedPeers: [] },
        client: null,
      })),
      shutdownDesktopClient: vi.fn(async () => undefined),
      startDesktopClient: vi.fn(() => restarted.promise),
    };
    const { result } = renderHook(() =>
      useDesktopClientLifecycle({
        api,
        supportsManagedServer: false,
        refreshSession: vi.fn(async () => null),
        selectedSessionIdRef,
        setError: vi.fn(),
        setSession,
      } as unknown as Parameters<typeof useDesktopClientLifecycle>[0]),
    );
    await waitFor(() => expect(api.fetchDesktopSnapshot).toHaveBeenCalled());
    setSession.mockClear();

    let restart!: Promise<void>;
    act(() => {
      restart = result.current.restartDesktopClient("test");
    });
    await waitFor(() => expect(api.startDesktopClient).toHaveBeenCalled());
    selectedSessionIdRef.current = "newly-selected-session";
    restarted.resolve({
      bootstrap: { clientStateExists: true, savedPeers: [] },
      client: null,
    });
    await act(async () => restart);

    expect(setSession).not.toHaveBeenCalledWith(null);
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
    const { result } = renderHook(() =>
      useDesktopClientLifecycle({
        api,
        supportsManagedServer: false,
        refreshSession: vi.fn(async () => null),
        selectedSessionIdRef: { current: null },
        setError: vi.fn(),
        setSession: vi.fn(),
      } as unknown as Parameters<typeof useDesktopClientLifecycle>[0]),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.ensureDesktopClientStarted());
    expect(result.current.startupPhase).toBe("client-error");
    await act(async () => result.current.refreshSnapshot());
    expect(result.current.startupPhase).toBe("ready");
    expect(result.current.snapshot).toBe(ready);
  });
});
