import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { useDesktopClientLifecycle } from "../src/hooks/useDesktopClientLifecycle";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

describe("desktop client restart selection ordering", () => {
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
