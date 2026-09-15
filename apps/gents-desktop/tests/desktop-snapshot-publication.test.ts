import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  BackendSaveRequest,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createDesktopShellConfigActions } from "../src/hooks/desktopShellConfigActions";
import { useDesktopClientLifecycle } from "../src/hooks/useDesktopClientLifecycle";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((next, fail) => {
    resolve = next;
    reject = fail;
  });
  return { promise, reject, resolve };
}

function snapshot(label: string): DesktopClientSnapshot {
  return {
    bootstrap: { clientStateExists: false, savedPeers: [] },
    client: null,
    label,
  } as unknown as DesktopClientSnapshot;
}

function renderLifecycle(api: DesktopApiAdapter) {
  return renderHook(() =>
    useDesktopClientLifecycle({
      api,
      supportsManagedServer: false,
      refreshSession: vi.fn(async () => null),
      selectedSessionIdRef: { current: null },
      setError: vi.fn(),
      setSession: vi.fn(),
    }),
  );
}

function configActions(
  api: DesktopApiAdapter,
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>,
) {
  return createDesktopShellConfigActions({
    api,
    mutateSnapshot,
    setError: vi.fn(),
    setSavingBehaviorConfig: vi.fn(),
    setSavingConfig: vi.fn(),
    setSelectedAgentDid: vi.fn(),
    setSelectedBehaviorId: vi.fn(),
  });
}

describe("desktop snapshot publication", () => {
  it("publishes authoritative reads after crossed saves and returns each mutation result", async () => {
    const firstSave = deferred<DesktopClientSnapshot>();
    const secondSave = deferred<DesktopClientSnapshot>();
    const authoritative = snapshot("authoritative-current");
    const fetchDesktopSnapshot = vi
      .fn()
      .mockResolvedValueOnce(snapshot("initial"))
      .mockResolvedValue(authoritative);
    const saveBackendConfig = vi
      .fn()
      .mockReturnValueOnce(firstSave.promise)
      .mockReturnValueOnce(secondSave.promise);
    const api = {
      fetchDesktopSnapshot,
      saveBackendConfig,
    } as unknown as DesktopApiAdapter;
    const { result } = renderLifecycle(api);
    await waitFor(() => expect(result.current.loading).toBe(false));
    const actions = configActions(api, result.current.mutateSnapshot);

    const older = actions.onSaveBackendConfig({} as BackendSaveRequest);
    const newer = actions.onSaveBackendConfig({} as BackendSaveRequest);
    const newerPayload = snapshot("newer-stale-payload");
    let newerResult: DesktopClientSnapshot | undefined;
    await act(async () => {
      secondSave.resolve(newerPayload);
      newerResult = await newer;
    });
    expect(newerResult).toBe(newerPayload);
    expect(result.current.snapshot).toBe(authoritative);

    const olderPayload = snapshot("older-stale-payload");
    let olderResult: DesktopClientSnapshot | undefined;
    await act(async () => {
      firstSave.resolve(olderPayload);
      olderResult = await older;
    });
    expect(olderResult).toBe(olderPayload);
    expect(result.current.snapshot).toBe(authoritative);
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(3);
  });

  it("keeps an already-issued read publishable when a mutation fails", async () => {
    const pendingRead = deferred<DesktopClientSnapshot>();
    const failedSave = deferred<DesktopClientSnapshot>();
    const fetchDesktopSnapshot = vi
      .fn()
      .mockResolvedValueOnce(snapshot("initial"))
      .mockReturnValueOnce(pendingRead.promise);
    const api = {
      fetchDesktopSnapshot,
      saveBackendConfig: vi.fn(() => failedSave.promise),
    } as unknown as DesktopApiAdapter;
    const { result } = renderLifecycle(api);
    await waitFor(() => expect(result.current.loading).toBe(false));
    const actions = configActions(api, result.current.mutateSnapshot);

    let reading!: Promise<void>;
    act(() => {
      reading = result.current.refreshSnapshot();
    });
    const mutation = actions.onSaveBackendConfig({} as BackendSaveRequest);
    failedSave.reject(new Error("write rejected"));
    await expect(mutation).rejects.toThrow("write rejected");

    const observed = snapshot("read-after-failure");
    pendingRead.resolve(observed);
    await act(async () => reading);
    expect(result.current.snapshot).toBe(observed);
  });
});
