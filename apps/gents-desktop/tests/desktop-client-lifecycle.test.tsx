import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { useDesktopClientLifecycle } from "../src/hooks/useDesktopClientLifecycle";

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
  it("does not let an older startup refresh replace a newer client-start snapshot", async () => {
    const refresh = deferred<Record<string, unknown>>();
    const start = deferred<Record<string, unknown>>();
    const selectedSessionIdRef = { current: null as string | null };
    const api = {
      fetchDesktopSnapshot: vi.fn(() => refresh.promise),
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
    const newer = {
      bootstrap: { clientStateExists: true, savedPeers: [] },
      client: {},
    };
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
});
