import { waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  BackendSaveRequest,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createDesktopShellConfigActions } from "../src/hooks/desktopShellConfigActions";
import { lifecycleFor } from "./shell-fixture";

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

function configActions(
  api: DesktopApiAdapter,
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>,
) {
  return createDesktopShellConfigActions({
    api,
    mutateSnapshot,
    reportFailure: vi.fn(),
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
    const lifecycle = lifecycleFor(api);
    await waitFor(() => expect(lifecycle.snapshot).not.toBeNull());
    const actions = configActions(api, lifecycle.mutateSnapshot);

    const older = actions.changeConfig("saveBackendConfig", {} as BackendSaveRequest);
    const newer = actions.changeConfig("saveBackendConfig", {} as BackendSaveRequest);
    const newerPayload = snapshot("newer-stale-payload");
    let newerResult: DesktopClientSnapshot | undefined;
    secondSave.resolve(newerPayload);
    newerResult = await newer;
    expect(newerResult).toBe(newerPayload);
    expect(lifecycle.snapshot).toEqual(authoritative);

    const olderPayload = snapshot("older-stale-payload");
    let olderResult: DesktopClientSnapshot | undefined;
    firstSave.resolve(olderPayload);
    olderResult = await older;
    expect(olderResult).toBe(olderPayload);
    expect(lifecycle.snapshot).toEqual(authoritative);
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
    const lifecycle = lifecycleFor(api);
    await waitFor(() => expect(lifecycle.snapshot).not.toBeNull());
    const actions = configActions(api, lifecycle.mutateSnapshot);

    const reading = lifecycle.refreshSnapshot();
    const mutation = actions.changeConfig(
      "saveBackendConfig",
      {} as BackendSaveRequest,
    );
    failedSave.reject(new Error("write rejected"));
    await expect(mutation).rejects.toThrow("write rejected");

    const observed = snapshot("read-after-failure");
    pendingRead.resolve(observed);
    await reading;
    expect(lifecycle.snapshot).toEqual(observed);
  });
});
